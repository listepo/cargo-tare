//! `cargo tare seed`: give a fresh checkout a target dir cloned from a sibling's. On APFS
//! `fs::copy` is `clonefile`, so the copy shares every block with its source until one of them
//! is rewritten: a new worktree starts with a warm target and costs no disk space.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::engine::ProfileLock;
use crate::index::HashIndex;
use crate::inventory;
use crate::model::{self, CARGO_LOCK_FILE, ST_BLOCK_BYTES, Stamp, TMP_PREFIX};

/// Cargo's own name for the incremental cache; seeding it would copy a cache that belongs to
/// another checkout's build and that cargo will not use.
const INCREMENTAL: &str = "incremental";
/// The target dir of a checkout, as cargo names it by default.
pub const TARGET: &str = "target";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seeded {
    pub source: PathBuf,
    pub files: usize,
    pub symlinks: usize,
    /// Allocated bytes of what was copied, as `du` reports it. The clones share these blocks
    /// with the source, so the volume loses nothing; this is what the new target will appear
    /// to weigh.
    pub bytes: u64,
    /// Profile dirs of the source that a build held; nothing under them was copied.
    pub busy: Vec<PathBuf>,
}

/// The dir of the checkout `dir` belongs to: the nearest one above it holding a `.git`.
fn checkout_root(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|above| above.join(".git").exists())
}

/// The best target to seed from: in the sibling checkouts of the same repository, at the same
/// place inside them as `checkout` is inside its own, the one built most recently. `None` when
/// the checkout has no repository, no siblings, or none of them has a target there.
pub fn choose(checkout: &Path) -> Option<PathBuf> {
    let root = checkout_root(checkout)?;
    // A workspace can sit anywhere inside a checkout; its sibling sits in the same place.
    let relative = checkout.strip_prefix(root).ok()?;
    let common = inventory::family(&checkout.join(TARGET))?;
    let mut best: Option<(u64, PathBuf)> = None;
    for sibling in inventory::checkouts(&common) {
        let sibling = sibling.canonicalize().unwrap_or(sibling);
        if sibling == root {
            continue;
        }
        let target = sibling.join(relative).join(TARGET);
        let Ok(profiles) = model::profile_dirs(&target) else {
            continue;
        };
        let built = profiles
            .iter()
            .filter_map(|dir| inventory::last_built(dir))
            .max();
        // A target nobody ever built is still better than nothing, hence `unwrap_or(0)`.
        let built = built.unwrap_or(0);
        if best.as_ref().is_none_or(|(seen, _)| built > *seen) {
            best = Some((built, target));
        }
    }
    best.map(|(_, target)| target)
}

/// Copies `source` (a target dir) to `<checkout>/target`. The destination must not exist yet:
/// this seeds a fresh checkout and never merges into a target somebody is already using.
///
/// The index gets the copies of every source file it already knows, marked shared on both
/// sides, so the next dedupe run leaves the pair alone.
pub fn seed(
    checkout: &Path,
    source: &Path,
    index: &mut HashIndex,
    dry_run: bool,
) -> io::Result<Seeded> {
    let target = checkout.join(TARGET);
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already has a target dir", checkout.display()),
        ));
    }
    // Cargo's lock per source profile dir, held for the whole walk: a build writing into the
    // source would otherwise be copied half way.
    let mut locks = Vec::new();
    let mut seeded = Seeded {
        source: source.to_path_buf(),
        ..Seeded::default()
    };
    for dir in model::profile_dirs(source)? {
        match ProfileLock::try_acquire(&dir)? {
            Some(lock) => locks.push((dir, lock)),
            None => seeded.busy.push(dir),
        }
    }

    let skipped = |entry: &walkdir::DirEntry| {
        let name = entry.file_name();
        let busy = seeded.busy.iter().any(|dir| entry.path().starts_with(dir));
        busy || name == INCREMENTAL
            || name == CARGO_LOCK_FILE
            || name.to_string_lossy().starts_with(TMP_PREFIX)
    };
    let walk = WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !skipped(entry));
    for entry in walk {
        let entry = entry?;
        let Ok(relative) = entry.path().strip_prefix(source) else {
            continue;
        };
        let to = target.join(relative);
        let kind = entry.file_type();
        if kind.is_dir() {
            if !dry_run {
                fs::create_dir_all(&to)?;
            }
        } else if kind.is_symlink() {
            seeded.symlinks += 1;
            if !dry_run {
                symlink(fs::read_link(entry.path())?, &to)?;
            }
        } else {
            let metadata = entry.metadata()?;
            seeded.files += 1;
            seeded.bytes += metadata.blocks() * ST_BLOCK_BYTES;
            if !dry_run {
                // `clonefile` on APFS: the copy shares the blocks until one side is written.
                fs::copy(entry.path(), &to)?;
                register(index, &metadata, &to);
            }
        }
    }
    Ok(seeded)
}

/// Both sides of a clone hold the same bytes, so the copy inherits the source's hash and both
/// are marked shared. A source the index has never hashed stays unknown: seeding must not read
/// gigabytes to fill an index that the next `run` fills anyway.
fn register(index: &mut HashIndex, source: &fs::Metadata, copy: &Path) {
    let Ok(from) = Stamp::of(source) else {
        return;
    };
    let Some(hash) = index.get(&from).map(|entry| entry.hash) else {
        return;
    };
    index.mark_shared(&from);
    if let Ok(stamp) = Stamp::read(copy) {
        index.put(&stamp, hash, true);
    }
}
