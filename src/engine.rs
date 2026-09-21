//! `lock → scan → plan → apply → report`, and the safety invariants of `DESIGN.md`.
//!
//! The engine never decides *what* to replace, that is a pass's job, and it does not compare
//! contents. It guarantees *how*: under cargo's own lock, whole hardlink groups only, never over
//! a file that changed since the scan, and through a temp file plus `rename`. A pass that
//! rewrites content (compression) only ever gets private copies to work on.

use std::fs::{self, File, FileTimes, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use crate::model::{self, CARGO_LOCK_FILE, Inode, Profile, Stamp, TMP_PREFIX};
use crate::sys::{self, COMPRESSED};

/// Copies handed to [`Pass::compress`] at once: bounds the work a crash throws away and the
/// time between checking a group and swapping its copy in.
const COMPRESS_BATCH: usize = 256;

/// How a replacement gets its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Share {
    /// A copy-on-write clone: its own inode and its own metadata, so a rewrite of either name
    /// touches only that name. The only safe choice for anything a build rewrites.
    Clone,
    /// A hardlink: one inode under every name. Cheap everywhere, including filesystems with no
    /// copy-on-write at all, and dangerous for exactly one reason — rustc opens its outputs with
    /// truncate, so a rebuild rewrites the inode in place and every other name with it. The
    /// pass decides where that is acceptable; the engine only refuses to change a file's mode
    /// on the way.
    Link,
}

/// Replace every path of `member` with `source`, shared the way `how` says.
#[derive(Clone, Debug)]
pub struct Replace {
    pub source: PathBuf,
    pub source_stamp: Stamp,
    pub member: Inode,
    pub how: Share,
}

#[derive(Clone, Debug)]
pub enum Action {
    Replace(Replace),
    /// Replace every path of the inode with a compressed copy of itself.
    Compress(Inode),
    /// Lossy: delete a profile dir whose lock we hold, the way `cargo clean` would, or a dir
    /// inside one, such as `incremental/`. `reason` is for the report, dry run included.
    Remove {
        dir: PathBuf,
        reason: String,
    },
    /// Lossy: delete `dir`, which lies inside `target` but outside its profile dirs — the whole
    /// target itself (`doc/` and `CACHEDIR.TAG` included) or something beside them, such as
    /// `doc/` alone. Applied only when a profile dir we hold the lock for is inside `target` and
    /// no dir reported busy is: what guards such a removal is the target's own build locks.
    /// `bytes` is the caller's own measurement, because the engine only scans profile dirs.
    RemoveTarget {
        target: PathBuf,
        dir: PathBuf,
        reason: String,
        bytes: u64,
    },
}

pub trait Pass {
    fn name(&self) -> &'static str;
    /// Lossy passes delete rebuildable data and run only when named in [`Options::lossy`].
    fn lossy(&self) -> bool {
        false
    }
    /// Sees only profiles whose lock is held. May read files, must not change anything.
    fn plan(&self, profiles: &[Profile]) -> Vec<Action>;
    /// Called after `replace` was applied; `new` is the stamp of the inode now at its paths.
    fn replaced(&self, _replace: &Replace, _new: &Stamp) {}
    /// For a pass that plans [`Action::Compress`]: compress these files where they are. They
    /// are private copies with one link each; one that comes back without the compressed flag
    /// is thrown away and its group stays as it was.
    fn compress(&self, _copies: &[PathBuf]) {}
    /// Told to every pass, not only the planning one: the content of inode `old` now lives,
    /// byte for byte, in the unshared inode `new`.
    fn rewritten(&self, _old: &Stamp, _new: &Stamp) {}
}

#[derive(Debug, Default)]
pub struct Options {
    pub dry_run: bool,
    pub lossy: Vec<String>,
}

/// Why a planned group was left alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    /// A path lies outside the profile dirs we hold the lock for.
    Unlocked,
    /// The inode has links outside the profile dir; replacing ours would split the group.
    ForeignLinks,
    /// Flags other than `COMPRESSED` (immutable, append-only, …) are not ours to drop.
    Flags,
    CrossDevice,
    SameInode,
    SizeMismatch,
    /// A hardlink would put one mode on both names, and these two do not agree on one.
    ModeMismatch,
    /// Size, mtime or inode differs from the scan: cargo or rustc got there first.
    Changed,
    /// The backend left the copy uncompressed: not worth it, not supported, or an error there.
    NotCompressed,
    Failed(io::ErrorKind),
}

#[derive(Debug, Default)]
pub struct PassReport {
    pub name: &'static str,
    pub planned: usize,
    pub planned_bytes: u64,
    pub applied: usize,
    /// Replaced inodes: their allocated bytes, an upper bound (a clone shares, it is not
    /// free). Compressed inodes: allocated bytes before minus after.
    pub freed_bytes: u64,
    pub skipped: Vec<(PathBuf, Skip)>,
    /// Every planned removal with its reason; on a dry run nothing of it happened. Planned
    /// minus skipped is what is gone.
    pub removals: Vec<(PathBuf, String)>,
}

impl PassReport {
    fn skip(&mut self, member: &Inode, skip: Skip) {
        let path = member.paths.first().cloned().unwrap_or_default();
        self.skipped.push((path, skip));
    }
}

#[derive(Debug, Default)]
pub struct Report {
    /// Profile dirs skipped because a build held their lock.
    pub busy: Vec<PathBuf>,
    pub temps_removed: usize,
    pub passes: Vec<PassReport>,
    /// Set when the run let go before its plan was done; the rest was never started.
    pub interrupted: Option<Interrupted>,
}

/// Why a run let go of its locks early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupted {
    /// The caller raised its stop flag.
    Stopped,
    /// The locks were held for as long as the caller allowed.
    OutOfBudget,
}

/// When a run has to let go, checked between two actions and between two compress batches:
/// every action is a whole replacement or removal, so what a run leaves behind is old or new,
/// never half of either.
#[derive(Clone, Copy, Debug, Default)]
pub struct Interrupt<'a> {
    pub stop: Option<&'a AtomicBool>,
    pub deadline: Option<Instant>,
}

impl Interrupt<'_> {
    fn due(&self) -> Option<Interrupted> {
        if self.stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            Some(Interrupted::Stopped)
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Some(Interrupted::OutOfBudget)
        } else {
            None
        }
    }
}

/// Delete `dir` whole and account for it, unless the caller's guard already refused it.
/// The locks of the profile dirs inside stay open until the run ends; their files are gone,
/// which is exactly what `cargo clean` leaves behind as well.
fn remove(
    dir: PathBuf,
    unlocked: bool,
    bytes: u64,
    locked: &mut Vec<PathBuf>,
    pass_report: &mut PassReport,
) {
    let skip = if unlocked {
        Some(Skip::Unlocked)
    } else {
        fs::remove_dir_all(&dir)
            .err()
            .map(|e| Skip::Failed(e.kind()))
    };
    match skip {
        None => {
            pass_report.applied += 1;
            pass_report.freed_bytes += bytes;
            locked.retain(|kept| !kept.starts_with(&dir));
        }
        Some(skip) => pass_report.skipped.push((dir, skip)),
    }
}

/// Cargo's own per-profile lock, held exclusively until dropped.
pub struct ProfileLock {
    _file: File,
}

impl ProfileLock {
    /// `None` when a build holds the lock. A missing lock file is an error: not a profile dir.
    pub fn try_acquire(profile_dir: &Path) -> io::Result<Option<Self>> {
        Self::try_lock_file(&profile_dir.join(CARGO_LOCK_FILE))
    }

    /// The same, for a lock file named directly — `$CARGO_HOME/.package-cache`, which cargo
    /// holds for every dir of the cargo home at once.
    pub fn try_lock_file(file: &Path) -> io::Result<Option<Self>> {
        let file = File::options().read(true).write(true).open(file)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }
}

/// Which lock guards the dirs of a run.
#[derive(Clone, Copy, Debug)]
pub enum Locks<'a> {
    /// Cargo's per-profile lock: each dir carries its own `.cargo-lock`, and a dir whose lock a
    /// build holds is left out of the run.
    PerDir,
    /// One lock file for every dir at once, the way cargo guards its home with
    /// `.package-cache`. Held: every dir is worked on. Busy: none of them is.
    Shared(&'a Path),
}

pub fn run(
    profile_dirs: &[PathBuf],
    passes: &[&dyn Pass],
    opts: &Options,
    locks_of: Locks<'_>,
) -> io::Result<Report> {
    run_with(profile_dirs, passes, opts, locks_of, Interrupt::default())
}

/// [`run`], letting go early when `interrupt` says so.
pub fn run_with(
    profile_dirs: &[PathBuf],
    passes: &[&dyn Pass],
    opts: &Options,
    locks_of: Locks<'_>,
    interrupt: Interrupt<'_>,
) -> io::Result<Report> {
    // Sorted order, so two concurrent runs cannot take the same locks in opposite order.
    let mut dirs = profile_dirs.to_vec();
    dirs.sort();
    dirs.dedup();

    let mut report = Report::default();
    let mut locks = Vec::new();
    let mut locked = Vec::new();
    match locks_of {
        Locks::PerDir => {
            for dir in dirs {
                match ProfileLock::try_acquire(&dir)? {
                    Some(lock) => {
                        locks.push(lock);
                        locked.push(dir);
                    }
                    None => report.busy.push(dir),
                }
            }
        }
        Locks::Shared(file) => match ProfileLock::try_lock_file(file)? {
            Some(lock) => {
                locks.push(lock);
                locked = dirs;
            }
            None => report.busy = dirs,
        },
    }

    let mut profiles = scan_all(&locked)?;
    if !opts.dry_run {
        for temp in profiles.iter().flat_map(|p| &p.stale_temps) {
            fs::remove_file(temp)?;
            report.temps_removed += 1;
        }
    }

    for pass in passes {
        if pass.lossy() && !opts.lossy.iter().any(|name| name == pass.name()) {
            continue;
        }
        let mut pass_report = PassReport {
            name: pass.name(),
            ..PassReport::default()
        };
        let mut removal_tried = false;
        let mut to_compress = Vec::new();
        for action in pass.plan(&profiles) {
            if let Some(why) = interrupt.due() {
                report.interrupted = Some(why);
                break;
            }
            let bytes = match &action {
                Action::Replace(replace) => replace.member.allocated,
                Action::Compress(inode) => inode.allocated,
                Action::Remove { dir, reason } => {
                    pass_report.removals.push((dir.clone(), reason.clone()));
                    profiles
                        .iter()
                        .filter(|profile| dir.starts_with(&profile.dir))
                        .flat_map(|profile| &profile.inodes)
                        // A link from outside survives the removal, so nothing is freed by it.
                        .filter(|inode| inode.paths.iter().all(|path| path.starts_with(dir)))
                        .map(|inode| inode.allocated)
                        .sum()
                }
                Action::RemoveTarget {
                    dir, reason, bytes, ..
                } => {
                    pass_report.removals.push((dir.clone(), reason.clone()));
                    *bytes
                }
            };
            pass_report.planned += 1;
            pass_report.planned_bytes += bytes;
            if opts.dry_run {
                continue;
            }
            match action {
                Action::Compress(inode) => to_compress.push(inode),
                Action::Remove { dir, .. } => {
                    removal_tried = true;
                    // A locked profile dir or something inside it, never one around it.
                    let unlocked = !locked.iter().any(|held| dir.starts_with(held));
                    remove(dir, unlocked, bytes, &mut locked, &mut pass_report);
                }
                Action::RemoveTarget { target, dir, .. } => {
                    removal_tried = true;
                    // Ours only if the target holds still: we have a lock inside it, nothing in
                    // it is being built, and what goes is inside it.
                    let holds_lock = locked.iter().any(|held| held.starts_with(&target));
                    let building = report.busy.iter().any(|busy| busy.starts_with(&target));
                    let unlocked = !holds_lock || building || !dir.starts_with(&target);
                    remove(dir, unlocked, bytes, &mut locked, &mut pass_report);
                }
                Action::Replace(replace) => match apply_replace(&replace, &locked) {
                    None => {
                        pass_report.applied += 1;
                        pass_report.freed_bytes += replace.member.allocated;
                        // Under the lock nobody else can have touched the new inode yet.
                        pass.replaced(&replace, &Stamp::read(&replace.member.paths[0])?);
                    }
                    Some(skip) => pass_report.skip(&replace.member, skip),
                },
            }
        }
        for batch in to_compress.chunks(COMPRESS_BATCH) {
            if let Some(why) = interrupt.due() {
                report.interrupted = Some(why);
                break;
            }
            apply_compress(batch, &locked, *pass, passes, &mut pass_report);
        }
        if report.interrupted.is_some() {
            report.passes.push(pass_report);
            break;
        }
        // A removal that failed half way has changed the dir too.
        if pass_report.applied > 0 || removal_tried {
            // ponytail: full rescan so the next pass sees the new inodes; patch the model in
            // place if scan time ever shows up in the benchmarks.
            profiles = scan_all(&locked)?;
        }
        report.passes.push(pass_report);
    }
    drop(locks);
    Ok(report)
}

fn scan_all(dirs: &[PathBuf]) -> io::Result<Vec<Profile>> {
    dirs.iter().map(|dir| model::scan(dir)).collect()
}

fn apply_replace(replace: &Replace, locked: &[PathBuf]) -> Option<Skip> {
    try_replace(replace, locked).unwrap_or_else(|e| Some(Skip::Failed(e.kind())))
}

fn try_replace(replace: &Replace, locked: &[PathBuf]) -> io::Result<Option<Skip>> {
    let Replace {
        source,
        source_stamp,
        member,
        how: _,
    } = replace;
    if !is_locked(source, locked) {
        return Ok(Some(Skip::Unlocked));
    }
    if let Some(skip) = check_group(member, locked)? {
        return Ok(Some(skip));
    }
    if source_stamp.dev != member.stamp.dev {
        return Ok(Some(Skip::CrossDevice));
    }
    if source_stamp.ino == member.stamp.ino {
        return Ok(Some(Skip::SameInode));
    }
    if source_stamp.size != member.stamp.size {
        return Ok(Some(Skip::SizeMismatch));
    }
    if Stamp::read(source)? != *source_stamp {
        return Ok(Some(Skip::Changed));
    }

    let temp = sibling_temp(&member.paths[0]);
    match replace.how {
        Share::Clone => swap_in(&temp, member, || clone_as(source, &temp, member))?,
        Share::Link => {
            // One inode under both names means one mode for both: linking files whose
            // permissions differ would quietly change the other name's.
            if sys::mode(&fs::symlink_metadata(source)?) != member.mode {
                return Ok(Some(Skip::ModeMismatch));
            }
            swap_in(&temp, member, || link_as(source, &temp, member))?;
        }
    }
    Ok(None)
}

fn is_locked(path: &Path, locked: &[PathBuf]) -> bool {
    locked.iter().any(|dir| path.starts_with(dir))
}

/// What must hold for a group before any of its paths is replaced.
fn check_group(member: &Inode, locked: &[PathBuf]) -> io::Result<Option<Skip>> {
    if member.paths.is_empty() {
        return Ok(Some(Skip::Changed));
    }
    if !member.paths.iter().all(|path| is_locked(path, locked)) {
        return Ok(Some(Skip::Unlocked));
    }
    if member.nlink != member.paths.len() as u64 {
        return Ok(Some(Skip::ForeignLinks));
    }
    if member.flags & !COMPRESSED != 0 {
        return Ok(Some(Skip::Flags));
    }
    for path in &member.paths {
        if Stamp::read(path)? != member.stamp {
            return Ok(Some(Skip::Changed));
        }
    }
    Ok(None)
}

fn apply_compress(
    batch: &[Inode],
    locked: &[PathBuf],
    pass: &dyn Pass,
    passes: &[&dyn Pass],
    report: &mut PassReport,
) {
    let mut staged = Vec::new();
    for member in batch {
        match stage_copy(member, locked) {
            Ok(Ok(copy)) => staged.push((member, copy)),
            Ok(Err(skip)) => report.skip(member, skip),
            Err(e) => report.skip(member, Skip::Failed(e.kind())),
        }
    }
    let copies: Vec<PathBuf> = staged.iter().map(|(_, copy)| copy.clone()).collect();
    pass.compress(&copies);
    for (member, copy) in staged {
        match finish_compress(member, &copy, locked) {
            Ok(Ok(new)) => {
                report.applied += 1;
                report.freed_bytes += member.allocated.saturating_sub(new.allocated);
                for pass in passes {
                    pass.rewritten(&member.stamp, &new.stamp);
                }
            }
            Ok(Err(skip)) => report.skip(member, skip),
            Err(e) => report.skip(member, Skip::Failed(e.kind())),
        }
    }
}

/// A private copy of the group's content next to its first path. Costs no space: a clone.
fn stage_copy(member: &Inode, locked: &[PathBuf]) -> io::Result<Result<PathBuf, Skip>> {
    if let Some(skip) = check_group(member, locked)? {
        return Ok(Err(skip));
    }
    let copy = sibling_temp(&member.paths[0]);
    if let Err(e) = clone_as(&member.paths[0], &copy, member) {
        let _ = fs::remove_file(&copy);
        return Err(e);
    }
    Ok(Ok(copy))
}

/// Swaps a compressed copy in, or removes the copy. Returns the inode now at the paths.
fn finish_compress(
    member: &Inode,
    copy: &Path,
    locked: &[PathBuf],
) -> io::Result<Result<Inode, Skip>> {
    let outcome = try_finish_compress(member, copy, locked);
    if !matches!(outcome, Ok(Ok(_))) {
        let _ = fs::remove_file(copy);
    }
    outcome
}

fn try_finish_compress(
    member: &Inode,
    copy: &Path,
    locked: &[PathBuf],
) -> io::Result<Result<Inode, Skip>> {
    let compressed = Inode::read(copy)?;
    if compressed.flags & COMPRESSED == 0 || compressed.stamp.size != member.stamp.size {
        return Ok(Err(Skip::NotCompressed));
    }
    // The backend took its time: look at the group once more.
    if let Some(skip) = check_group(member, locked)? {
        return Ok(Err(skip));
    }
    swap_in(copy, member, || restore_meta(copy, member))?;
    Inode::read(&member.paths[0]).map(Ok)
}

/// Puts the inode that `prepare` leaves at `temp` at every path of `member`: `rename` over the
/// first path, a `hard_link` plus `rename` for each other one. A crash leaves, per path, the old
/// file or the new one, never a partial file. A group cut in half still holds identical bytes
/// and the next run joins it again.
fn swap_in(
    temp: &Path,
    member: &Inode,
    prepare: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let Some((first, rest)) = member.paths.split_first() else {
        return Ok(());
    };
    rename_over(temp, first, prepare)?;
    for path in rest {
        let temp = sibling_temp(path);
        rename_over(&temp, path, || fs::hard_link(first, &temp))?;
    }
    Ok(())
}

/// Runs `prepare`, then renames `temp` over `path`; removes `temp` when either step fails.
fn rename_over(
    temp: &Path,
    path: &Path,
    prepare: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let result = prepare().and_then(|()| fs::rename(temp, path));
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// A hardlink instead of a clone. There is no metadata to restore — the inode is the source's
/// and so are its mode and times — except that the shared inode keeps the *later* of the two
/// modification times. A file that suddenly reads older than what it was built from is a file
/// cargo rebuilds, and that would make the pass cost a build instead of saving space. The
/// source's own mtime moves forward with it, which is the safe direction.
fn link_as(source: &Path, temp: &Path, member: &Inode) -> io::Result<()> {
    fs::hard_link(source, temp)?;
    if member.stamp.mtime > fs::symlink_metadata(temp)?.modified()? {
        File::open(temp)?.set_times(FileTimes::new().set_modified(member.stamp.mtime))?;
    }
    Ok(())
}

fn clone_as(source: &Path, temp: &Path, member: &Inode) -> io::Result<()> {
    sys::clone_file(source, temp)?;
    restore_meta(temp, member)
}

fn restore_meta(temp: &Path, member: &Inode) -> io::Result<()> {
    // Times first: a read-only mode would not stop `futimens`, but an unreadable one stops `open`.
    File::open(temp)?.set_times(FileTimes::new().set_modified(member.stamp.mtime))?;
    sys::set_mode(temp, member.mode)
}

fn sibling_temp(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "{TMP_PREFIX}{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    path.with_file_name(name)
}
