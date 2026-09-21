//! One run of the tool, whoever starts it. The CLI and the daemon are front ends that build a
//! [`Request`], call a [`Session`] and show what it returns; everything the tool *does* happens
//! here. The session never prints, never exits and never reads the environment or a config file
//! on its own: [`Settings`] carries the paths, and the helpers that resolve them from the
//! environment ([`default_index`], [`crate::config::default_path`], [`crate::cargo_home::path`])
//! are functions a front end calls.
//!
//! Two sessions that change anything — `plan`, `apply` and `seed` — are kept apart by the run
//! lock, a file next to the hash index; the second one gets [`Error::RunLockHeld`]. That is the
//! whole coordination between a manual run and the daemon: no IPC.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use crate::advise::{self, Finding, Kind, Note};
use crate::cargo_home;
use crate::compress::{self, Compress};
use crate::config::Config;
use crate::dedupe::{self, Dedupe};
use crate::doc::{self, Doc, Docs};
use crate::engine::{self, Interrupt, Interrupted, Locks, Options, Pass, Report};
use crate::error::{Error, Result};
use crate::evict::{self, Evict, Limits};
use crate::incremental::{self, Incremental};
use crate::index::HashIndex;
use crate::inventory::{self, Inventory, ProfileInfo};
use crate::orphans::{self, Orphan, Orphans};
use crate::seed::{self, Seeded};

/// Relative to `$HOME`. The digit follows the index file format.
const DEFAULT_INDEX: &str = ".cache/dunnage/hashes-v1.bin";
/// The run lock's file name, in the hash index's dir.
pub const RUN_LOCK: &str = "run.lock";
/// What a report calls the one group `across_families` makes, in place of a family dir.
pub const ACROSS_FAMILIES: &str = "<across families>";
/// Every pass that deletes rebuildable data; `lossy` takes these names.
pub const LOSSY_PASSES: [&str; 4] = [orphans::NAME, evict::NAME, incremental::NAME, doc::NAME];
/// Every pass, in pipeline order; `passes` takes these names.
pub const PASSES: [&str; 6] = [
    orphans::NAME,
    evict::NAME,
    incremental::NAME,
    doc::NAME,
    compress::NAME,
    dedupe::NAME,
];

/// `$HOME/.cache/dunnage/hashes-v1.bin`; `None` without a `$HOME`.
pub fn default_index() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(DEFAULT_INDEX))
}

/// Where a session keeps its state. The default names no file, which is enough for `inventory`
/// and `advise`: they keep no state. `plan`, `apply` and `seed` refuse it.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// The content-hash cache; the run lock sits next to it.
    pub index: PathBuf,
}

impl Settings {
    pub fn run_lock(&self) -> PathBuf {
        self.index.with_file_name(RUN_LOCK)
    }
}

/// What a run is asked to do, already merged from flags and config by the front end.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// Dirs to search; a target dir itself works too.
    pub roots: Vec<PathBuf>,
    /// Only these passes; empty is every pass `lossy` does not gate.
    pub passes: Vec<String>,
    /// Lossy passes to enable.
    pub lossy: Vec<String>,
    pub evict: Limits,
    /// Remove a target dir itself once `evict` took every profile dir of it.
    pub evict_whole_target: bool,
    pub incremental_idle_days: Option<u64>,
    /// Leave younger files alone; `None` keeps each pass's default.
    pub min_age: Option<Duration>,
    /// Leave smaller files alone; `None` keeps each pass's default.
    pub min_size: Option<u64>,
    /// Also compress this cargo home's unpacked sources, under its own lock.
    pub cargo_home: Option<PathBuf>,
    /// One group for every target instead of one per family.
    pub across_families: bool,
    /// Where the filesystem cannot share blocks, share build artifacts as hardlinks.
    pub link_artifacts: bool,
    /// Families to leave alone, by their dir.
    pub skip_families: Vec<PathBuf>,
}

impl Request {
    /// What the config file asks for when no flag says otherwise.
    pub fn from_config(config: &Config) -> Self {
        Self {
            roots: config.roots.clone(),
            lossy: config.lossy.clone(),
            evict: Limits {
                idle_days: config.evict.idle_days,
                max_total_bytes: config.evict.max_total_gib.map(gib_to_bytes),
            },
            evict_whole_target: config.evict.whole_target,
            incremental_idle_days: config.incremental.idle_days,
            min_age: config.min_age.map(Duration::from_secs),
            min_size: config.min_size,
            across_families: config.across_families,
            skip_families: config
                .family
                .iter()
                .filter(|(_, family)| family.skip)
                .map(|(dir, _)| dir.clone())
                .collect(),
            ..Self::default()
        }
    }

    /// Whether this request can run at all: known pass names, and every lossy pass with the
    /// threshold it needs.
    pub fn check(&self) -> Result<()> {
        for name in &self.lossy {
            ensure(LOSSY_PASSES.contains(&name.as_str()), || {
                format!("unknown lossy pass `{name}`")
            })?;
        }
        for name in &self.passes {
            ensure(PASSES.contains(&name.as_str()), || {
                format!("unknown pass `{name}`")
            })?;
        }
        ensure(
            self.enables(evict::NAME) != (self.evict == Limits::default()),
            || {
                "`--lossy evict` and a limit (--evict-idle-days, --evict-max-total-gib) need each other"
                    .into()
            },
        )?;
        ensure(
            self.enables(incremental::NAME) == self.incremental_idle_days.is_some(),
            || "`--lossy incremental` and `--incremental-idle-days` need each other".into(),
        )
    }

    fn enables(&self, lossy: &str) -> bool {
        self.lossy.iter().any(|name| name == lossy)
    }
}

pub fn gib_to_bytes(gib: u64) -> u64 {
    gib.saturating_mul(1 << 30)
}

fn ensure(ok: bool, message: impl FnOnce() -> String) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(message()))
    }
}

/// What a front end hears while a run goes on. Every method has a default that does nothing.
pub trait Observer {
    /// A group of targets is about to be worked on: a family, [`ACROSS_FAMILIES`], or a cargo
    /// home.
    fn group(&self, _group: &Path) {}
    /// The group is done, or let go early (`report.interrupted`).
    fn report(&self, _group: &Path, _report: &Report) {}
}

/// An observer that hears nothing.
pub struct Quiet;

impl Observer for Quiet {}

/// How a run is steered from outside while it goes on.
pub struct Control<'a> {
    pub observer: &'a dyn Observer,
    /// Raised, the run finishes the action it is on and returns; nothing new is started.
    pub stop: Option<&'a AtomicBool>,
    /// How long a group's build locks may be held. A group that runs out lets go, so a build
    /// waiting on a lock gets it, and is visited once more after the other groups; what is left
    /// then waits for the next run. `None` holds them until the group is done.
    pub lock_budget: Option<Duration>,
}

impl Default for Control<'_> {
    fn default() -> Self {
        Self {
            observer: &Quiet,
            stop: None,
            lock_budget: None,
        }
    }
}

impl Control<'_> {
    fn interrupt(&self) -> Interrupt<'_> {
        Interrupt {
            stop: self.stop,
            deadline: self.lock_budget.map(|budget| Instant::now() + budget),
        }
    }

    fn stopped(&self) -> bool {
        self.stop
            .is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// What a whole run did, group by group.
#[derive(Debug, Default)]
pub struct RunReport {
    pub dry_run: bool,
    /// In the order they ran. A group that ran out of lock budget appears twice.
    pub groups: Vec<(PathBuf, Report)>,
    pub compress_notes: Vec<String>,
    pub files_hashed: usize,
    /// Something was left for a later run: a build held a lock, or the budget ran out twice.
    pub left_busy: bool,
    /// The stop flag ended the run.
    pub stopped: bool,
}

/// What `advise` found.
#[derive(Debug, Default)]
pub struct Advice {
    pub findings: Vec<Finding>,
    pub notes: Vec<Note>,
    /// Files that are there and could not be read as TOML, with the parser's word for it.
    pub warnings: Vec<String>,
}

/// What `seed` did, and to which target.
#[derive(Debug)]
pub struct Seeding {
    /// The target dir that was filled.
    pub target: PathBuf,
    pub seeded: Seeded,
}

pub struct Session {
    settings: Settings,
}

impl Session {
    pub fn open(settings: Settings) -> Self {
        Self { settings }
    }

    /// Every cargo target dir under `roots`, and the cargo home's sources when one is named.
    /// Read-only; needs no run lock.
    pub fn inventory(&self, roots: &[PathBuf], cargo_home: Option<&Path>) -> Result<Inventory> {
        let mut inventory = read_inventory(roots)?;
        if let Some(home) = cargo_home {
            inventory.cargo_home = Some(cargo_home::inspect(home)?);
        }
        Ok(inventory)
    }

    /// What the manifests and cargo configs under `roots` and the cargo home's config make
    /// bigger than it needs to be. Reads text only; needs no run lock.
    pub fn advise(&self, roots: &[PathBuf], cargo_home: Option<&Path>) -> Result<Advice> {
        let inventory = read_inventory(roots)?;
        let mut advice = Advice::default();
        let mut read = Vec::new();
        for target in &inventory.targets {
            let Some(project) = target.root.parent() else {
                continue;
            };
            let files = [
                (project.join("Cargo.toml"), Kind::Manifest),
                (project.join(".cargo/config.toml"), Kind::Config),
            ];
            for (file, kind) in files {
                if read.contains(&file) {
                    continue;
                }
                review_file(&file, kind, project, &mut advice);
                read.push(file);
            }
        }
        // The cargo home is advised on even when it has no config file at all: the keys it is
        // missing are the point.
        if let Some(home) = cargo_home {
            review_file(&home.join("config.toml"), Kind::Home, home, &mut advice);
        }
        advice.notes = advise::notes(&inventory.targets);
        Ok(advice)
    }

    /// What [`apply`](Self::apply) would do, changing nothing but the hash cache.
    pub fn plan(&self, request: &Request, control: &Control) -> Result<RunReport> {
        self.run(request, control, true)
    }

    /// Plan and apply the passes, one group of targets at a time.
    pub fn apply(&self, request: &Request, control: &Control) -> Result<RunReport> {
        self.run(request, control, false)
    }

    /// Fill the empty target of `checkout` from `from` (a checkout or a target dir), or from the
    /// family's most recently built target.
    pub fn seed(&self, checkout: &Path, from: Option<&Path>, dry_run: bool) -> Result<Seeding> {
        let checkout = canonical(checkout)?;
        let source = match from {
            Some(path) => {
                let path = canonical(path)?;
                // A checkout or its target dir; both are what somebody means by "from there".
                let target = path.join(seed::TARGET);
                if target.is_dir() { target } else { path }
            }
            None => seed::choose(&checkout).ok_or_else(|| {
                Error::Invalid(
                    "no other checkout of this repository has a target dir; name one with --from"
                        .into(),
                )
            })?,
        };
        let _lock = self.lock()?;
        let mut hashes = HashIndex::load(&self.settings.index);
        let seeded = seed::seed(&checkout, &source, &mut hashes, dry_run).map_err(Error::at(
            format_args!("seeding {} from {}", checkout.display(), source.display()),
        ))?;
        if !dry_run {
            self.save(&hashes)?;
        }
        Ok(Seeding {
            target: checkout.join(seed::TARGET),
            seeded,
        })
    }

    fn run(&self, request: &Request, control: &Control, dry_run: bool) -> Result<RunReport> {
        request.check()?;
        let opts = Options {
            dry_run,
            lossy: request.lossy.clone(),
        };
        // `cargo_home` is a run of its own: it needs no target and no root.
        let only_home = request.roots.is_empty() && request.cargo_home.is_some();
        ensure(!request.roots.is_empty() || only_home, || {
            "no roots: name them on the command line or set `roots` in the config file".into()
        })?;
        let inventory = if only_home {
            Inventory::default()
        } else {
            read_inventory(&request.roots)?
        };
        ensure(!inventory.targets.is_empty() || only_home, || {
            "no cargo target dirs found".into()
        })?;
        // Everything above only reads. From here on the index is loaded and saved, and the passes
        // change targets: one session at a time.
        let _lock = self.lock()?;
        let index = RefCell::new(HashIndex::load(&self.settings.index));
        let (mut compress, mut dedupe) = (Compress::new(&index), Dedupe::new(&index));
        // Artifacts are only linked when the user asks; the cargo home's unpacked sources are
        // always safe to link, because cargo replaces a source dir instead of rewriting its files.
        let mut home_dedupe = Dedupe::new(&index);
        dedupe.link_fallback = request.link_artifacts;
        home_dedupe.link_fallback = true;
        if let Some(min_age) = request.min_age {
            (compress.min_age, dedupe.min_age) = (min_age, min_age);
            home_dedupe.min_age = min_age;
        }
        if let Some(bytes) = request.min_size {
            (compress.min_size, dedupe.min_size) = (bytes, bytes);
            home_dedupe.min_size = bytes;
        }
        // The size cap is global, so eviction is decided over everything under the roots at once.
        let profiles: Vec<ProfileInfo> = inventory
            .targets
            .iter()
            .flat_map(|target| target.profiles.iter().cloned())
            .collect();
        let chosen = evict::select(&profiles, now_unix(), request.evict);
        let mut evict = Evict::new(chosen.clone());
        if request.evict_whole_target {
            evict = evict.whole(evict::whole_targets(&inventory.targets, &chosen));
        }
        // Cargo keeps `incremental/` for workspace members only, so this costs one plain rebuild.
        let idle_days = request.incremental_idle_days.unwrap_or(u64::MAX);
        let incremental = Incremental::new(incremental::select(&profiles, now_unix(), idle_days));
        // Whole targets of checkouts git no longer registers; the sources next to them stay.
        let orphans = Orphans::new(
            inventory
                .targets
                .iter()
                .filter(|target| target.orphaned)
                .map(|target| Orphan {
                    target: target.root.clone(),
                    allocated_bytes: target.allocated_bytes,
                })
                .collect(),
        );
        // `cargo doc` writes this dir again from scratch and no build reads it.
        let docs = Doc::new(
            inventory
                .targets
                .iter()
                .filter(|target| target.doc_bytes > 0)
                .map(|target| Docs {
                    target: target.root.clone(),
                    allocated_bytes: target.doc_bytes,
                })
                .collect(),
        );
        // Pipeline order (`DESIGN.md`): orphans, evict, incremental, doc, compress, dedupe.
        let all: [&dyn Pass; 6] = [&orphans, &evict, &incremental, &docs, &compress, &dedupe];
        let passes: Vec<&dyn Pass> = all
            .into_iter()
            .filter(|pass| {
                request.passes.is_empty() || request.passes.iter().any(|name| name == pass.name())
            })
            .collect();
        // One group per family keeps a run's locks inside the repository it is working on. Across
        // families every target is compared with every other — unrelated projects do share
        // artifacts — and the price is that the locks of all of them are held for the whole run.
        let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for target in inventory.targets {
            let family = target.family.unwrap_or_else(|| target.root.clone());
            if request.skip_families.contains(&family) {
                continue;
            }
            // Not a path: the group is every family at once, and the report says so.
            let key = if request.across_families {
                PathBuf::from(ACROSS_FAMILIES)
            } else {
                family
            };
            let dirs = target.profiles.into_iter().map(|profile| profile.dir);
            groups.entry(key).or_default().extend(dirs);
        }

        let mut report = RunReport {
            dry_run,
            ..RunReport::default()
        };
        let mut again = Vec::new();
        for (group, profile_dirs) in &groups {
            if control.stopped() {
                break;
            }
            let done = visit(
                group,
                profile_dirs,
                &passes,
                &opts,
                Locks::PerDir,
                control,
                &mut report,
            )?;
            if done == Some(Interrupted::OutOfBudget) {
                again.push((group, profile_dirs));
            }
        }
        for (group, profile_dirs) in again {
            if control.stopped() {
                break;
            }
            let done = visit(
                group,
                profile_dirs,
                &passes,
                &opts,
                Locks::PerDir,
                control,
                &mut report,
            )?;
            report.left_busy |= done == Some(Interrupted::OutOfBudget);
        }
        // One more group, guarded by cargo's own home lock instead of per-profile locks. Only
        // `compress` runs here: these are unpacked sources, not build output.
        if let Some(home) = request.cargo_home.as_deref().filter(|_| !control.stopped()) {
            let dirs = cargo_home::dirs(home);
            ensure(!dirs.is_empty(), || {
                format!(
                    "no {} or {} in {}",
                    cargo_home::DIRS[0],
                    cargo_home::DIRS[1],
                    home.display()
                )
            })?;
            let lock = home.join(cargo_home::LOCK_FILE);
            ensure(lock.is_file(), || {
                format!(
                    "no {} in {}: cargo has never used it as its home",
                    cargo_home::LOCK_FILE,
                    home.display()
                )
            })?;
            // Compression, and sharing with the home's own policy: on a filesystem without
            // copy-on-write these sources are the one place a hardlink is safe.
            let home_passes: Vec<&dyn Pass> = passes
                .iter()
                .copied()
                .filter(|pass| pass.name() == compress::NAME)
                .chain(
                    passes
                        .iter()
                        .any(|pass| pass.name() == dedupe::NAME)
                        .then_some(&home_dedupe as &dyn Pass),
                )
                .collect();
            let done = visit(
                home,
                &dirs,
                &home_passes,
                &opts,
                Locks::Shared(&lock),
                control,
                &mut report,
            )?;
            report.left_busy |= done == Some(Interrupted::OutOfBudget);
        }
        report.stopped = control.stopped();
        report.compress_notes = compress.notes();
        report.files_hashed = dedupe.hashed();
        // The index only caches hashes of files as they are, so it is worth keeping on a dry run too.
        self.save(&index.borrow())?;
        Ok(report)
    }

    /// The run lock, held until dropped. Taken before the index is loaded, so no two sessions
    /// ever write the index over each other.
    fn lock(&self) -> Result<File> {
        ensure(!self.settings.index.as_os_str().is_empty(), || {
            "no hash index in the session's settings".into()
        })?;
        let path = self.settings.run_lock();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(Error::at(dir.display()))?;
        }
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(Error::at(path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => Err(Error::RunLockHeld(path)),
            Err(TryLockError::Error(error)) => Err(Error::at(path.display())(error)),
        }
    }

    fn save(&self, hashes: &HashIndex) -> Result<()> {
        let path = &self.settings.index;
        hashes
            .save(path)
            .map_err(Error::at(format_args!("saving {}", path.display())))
    }
}

/// One engine run over one group, told to the observer on both sides.
fn visit(
    group: &Path,
    dirs: &[PathBuf],
    passes: &[&dyn Pass],
    opts: &Options,
    locks: Locks<'_>,
    control: &Control,
    report: &mut RunReport,
) -> Result<Option<Interrupted>> {
    control.observer.group(group);
    let done = engine::run_with(dirs, passes, opts, locks, control.interrupt())?;
    control.observer.report(group, &done);
    report.left_busy |= !done.busy.is_empty();
    let interrupted = done.interrupted;
    report.groups.push((group.to_path_buf(), done));
    Ok(interrupted)
}

/// Canonical, so that families, scanned paths and locked dirs all compare equal.
fn read_inventory(roots: &[PathBuf]) -> Result<Inventory> {
    let roots = roots
        .iter()
        .map(|root| canonical(root))
        .collect::<Result<Vec<_>>>()?;
    Ok(inventory::inventory(&roots)?)
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(Error::at(path.display()))
}

/// One file's findings. A file that is not there is not a finding of its own — except for the
/// cargo home's config, whose absent keys `review` reports from an empty document.
fn review_file(file: &Path, kind: Kind, dir: &Path, advice: &mut Advice) {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(_) if kind == Kind::Home => String::new(),
        Err(_) => return,
    };
    let doc: toml::Table = match text.parse() {
        Ok(doc) => doc,
        Err(error) => {
            advice.warnings.push(format!("{}: {error}", file.display()));
            return;
        }
    };
    // Only asked when the answer matters, since it costs a process.
    let nightly = doc.get("unstable").is_some() && nightly_toolchain(dir);
    advice
        .findings
        .extend(advise::review(file, kind, &doc, nightly));
}

/// Whether the toolchain cargo would use in `dir` is a nightly one, which is the only one that
/// reads `[unstable]`. A rustc that cannot be run at all is treated as stable: the advice is
/// then about a key that does nothing, which is still the safer thing to say.
fn nightly_toolchain(dir: &Path) -> bool {
    std::process::Command::new("rustc")
        .arg("--version")
        .current_dir(dir)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("nightly"))
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_secs())
}
