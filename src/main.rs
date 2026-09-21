use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use dunnage::config::{self, Config};
use dunnage::eco::cargo::home as cargo_home;
use dunnage::engine;
use dunnage::inventory::{Inventory, Target};
use dunnage::session::{self, Control, Observer, Request, RunReport, Session, Settings};

const BYTES_PER_GIB: f64 = (1u64 << 30) as f64;
/// Exit code when a profile dir was skipped because a build holds its lock, or another run of
/// the tool holds the run lock.
const BUSY_EXIT: u8 = 2;
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// Shrink Cargo target directories without slowing builds
#[derive(Parser)]
#[command(name = "dunnage", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// The arguments, without the subcommand name cargo puts first: a `cargo-dunnage` link to this
/// binary is run by `cargo dunnage <args>` as `cargo-dunnage dunnage <args>`. No subcommand of
/// ours is called `dunnage`, so dropping it cannot take anything else away.
fn args() -> Vec<std::ffi::OsString> {
    let mut args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|first| first == "dunnage") {
        args.remove(1);
    }
    args
}

#[derive(Subcommand)]
enum Cmd {
    /// List cargo target dirs under the roots: real size, families, what the passes could win
    Status {
        /// Print the inventory as JSON
        #[arg(long)]
        json: bool,
        /// Also measure the cargo home's unpacked sources, which costs another walk
        /// [default: $CARGO_HOME, else ~/.cargo]
        #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "")]
        cargo_home: Option<PathBuf>,
        /// Dirs to search; a target dir itself works too [default: .]
        #[arg(value_name = "ROOT")]
        roots: Vec<PathBuf>,
    },
    /// Read the manifests and cargo configs under the roots and say what makes their targets
    /// bigger than they need to be; changes nothing
    Advise {
        /// Print the findings as JSON
        #[arg(long)]
        json: bool,
        /// Dirs to search; a target dir itself works too [default: .]
        #[arg(value_name = "ROOT")]
        roots: Vec<PathBuf>,
    },
    /// Plan and apply the passes, one family of targets at a time; profile dirs with a running
    /// build are skipped
    Run(RunArgs),
    /// Git worktrees that start warm
    #[command(subcommand)]
    Worktree(WorktreeCmd),
    /// Clone a sibling checkout's target into a fresh one, so its first build starts warm
    Seed {
        /// Where to copy from: a checkout or a target dir [default: the family's newest target]
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
        /// Report what would be copied without touching anything
        #[arg(long)]
        dry_run: bool,
        /// Content-hash cache [default: ~/.cache/dunnage/hashes-v1.bin]
        #[arg(long, value_name = "FILE")]
        index: Option<PathBuf>,
        /// The checkout to seed [default: .]
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum WorktreeCmd {
    /// `git worktree add`, then `seed` the new worktree from the repository's newest target
    Add {
        /// Seed as a dry run: the worktree is still added
        #[arg(long)]
        dry_run: bool,
        /// Content-hash cache [default: ~/.cache/dunnage/hashes-v1.bin]
        #[arg(long, value_name = "FILE")]
        index: Option<PathBuf>,
        /// Passed to `git worktree add` as they are
        #[arg(
            value_name = "GIT ARGS",
            required = true,
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        git_args: Vec<std::ffi::OsString>,
    },
}

#[derive(clap::Args)]
struct RunArgs {
    /// Report what would change without touching anything
    #[arg(long)]
    dry_run: bool,
    /// Enable a lossy pass (deletes rebuildable data); repeatable
    #[arg(long, value_name = "PASS")]
    lossy: Vec<String>,
    /// Run only this pass; repeatable [default: every pass not gated by `--lossy`]
    #[arg(long, value_name = "PASS")]
    pass: Vec<String>,
    /// With `--lossy evict`: remove profile dirs not built for this many days
    #[arg(long, value_name = "DAYS")]
    evict_idle_days: Option<u64>,
    /// With `--lossy evict`: then remove least recently built profile dirs until all targets
    /// under the roots fit into this many GiB
    #[arg(long, value_name = "GIB")]
    evict_max_total_gib: Option<u64>,
    /// With `--lossy evict`: once every profile dir of a target is evicted, remove the target dir
    /// itself, so `doc/`, `package/` and `tmp/` go with it
    #[arg(long)]
    evict_whole_target: bool,
    /// With `--lossy incremental`: drop the incremental cache of profile dirs not built for
    /// this many days
    #[arg(long, value_name = "DAYS")]
    incremental_idle_days: Option<u64>,
    /// Leave files younger than this alone, in seconds; both lossless passes [default: 3600]
    #[arg(long, value_name = "SECS")]
    min_age: Option<u64>,
    /// Leave files smaller than this alone; both lossless passes [default: 8192 / 4096]
    #[arg(long, value_name = "BYTES")]
    min_size: Option<u64>,
    /// Also compress the cargo home's unpacked sources (`registry/src`, `git/checkouts`)
    /// under cargo's own `.package-cache` lock [default: $CARGO_HOME, else ~/.cargo]
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "")]
    cargo_home: Option<PathBuf>,
    /// Also compress a content-addressed store: `GOCACHE`, `~/.cabal/store`, Zig's `o/`. No lock
    /// exists there, so only files older than an hour are touched. Repeatable
    #[arg(long, value_name = "DIR")]
    store: Vec<PathBuf>,
    /// Compare every target under the roots with every other, not only the targets of one
    /// repository: unrelated projects do share artifacts, at the price of one wider lock
    #[arg(long)]
    across_families: bool,
    /// HAZARD. Where the filesystem cannot share blocks (ext4, NTFS), let `dedupe` share equal
    /// build artifacts as hardlinks instead. rustc rewrites its outputs in place, so a later
    /// build that rewrites one linked artifact rewrites every other name for it, in every target
    /// sharing it. Off by default, and the cargo home's sources are shared without it
    #[arg(long)]
    link_artifacts: bool,
    /// Content-hash cache [default: ~/.cache/dunnage/hashes-v1.bin]
    #[arg(long, value_name = "FILE")]
    index: Option<PathBuf>,
    /// Configuration file [default: $XDG_CONFIG_HOME/dunnage/config.toml]
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Print the report as JSON instead of a table
    #[arg(long)]
    json: bool,
    /// Dirs to search; a target dir itself works too [default: `roots` from the config]
    #[arg(value_name = "ROOT")]
    roots: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let Cli { cmd } = Cli::parse_from(args());
    let done = match cmd {
        Cmd::Status {
            json,
            cargo_home,
            roots,
        } => status(json, cargo_home, roots).map(|()| Done::Everything),
        Cmd::Advise { json, roots } => advise(json, roots).map(|()| Done::Everything),
        Cmd::Run(args) => run(args),
        Cmd::Seed {
            from,
            dry_run,
            index,
            dir,
        } => seed_into(from, dry_run, index, dir),
        Cmd::Worktree(WorktreeCmd::Add {
            dry_run,
            index,
            git_args,
        }) => worktree_add(dry_run, index, &git_args),
    };
    match done {
        Ok(Done::Everything) => ExitCode::SUCCESS,
        // A cron job wants to tell "nothing to do" from "a build was in the way".
        Ok(Done::LeftBusy) => ExitCode::from(BUSY_EXIT),
        Err(error) => {
            eprintln!("error: {error:#}");
            // Another run is working on the same targets: the same "try again later".
            let busy = matches!(
                error.downcast_ref::<dunnage::Error>(),
                Some(dunnage::Error::RunLockHeld(_))
            );
            if busy {
                ExitCode::from(BUSY_EXIT)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// What a command finished with; the difference is visible in the exit code.
enum Done {
    Everything,
    LeftBusy,
}

impl Done {
    fn busy_if(left_busy: bool) -> Self {
        if left_busy {
            Self::LeftBusy
        } else {
            Self::Everything
        }
    }
}

/// `--cargo-home` without a value means "the one cargo would use".
fn home_flag(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.and_then(|flag| cargo_home::path((!flag.as_os_str().is_empty()).then_some(flag)))
}

/// The roots named on the command line, else `roots` from the config file, else `.`.
fn roots_or_config(roots: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let roots = if roots.is_empty() {
        match config::default_path() {
            Some(path) => Config::load(&path)?.roots,
            None => Vec::new(),
        }
    } else {
        roots
    };
    Ok(if roots.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        roots
    })
}

/// A session on `index`, or on the default one, kept as `config` says.
fn open(index: Option<PathBuf>, config: &Config) -> Result<Session> {
    let index = match index {
        Some(path) => path,
        None => session::default_index().context("HOME is not set; pass --index")?,
    };
    Ok(Session::open(Settings::from_config(index, config)))
}

/// The file `--config` names, which must be there, else the default one if there is one.
fn load_config(path: Option<&Path>) -> Result<Config> {
    Ok(match path {
        // A file the command line names and that is not there is a mistake, not a default.
        Some(path) => {
            ensure!(path.exists(), "no config file at {}", path.display());
            Config::load(path)?
        }
        None => match config::default_path() {
            Some(path) => Config::load(&path)?,
            None => Config::default(),
        },
    })
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / BYTES_PER_GIB)
}

/// What the filesystem under a target cannot do, in the words of the passes it silences.
/// `None` when it can do everything, which needs no line.
fn missing_caps(caps: &dunnage::sys::Caps) -> Option<&'static str> {
    match (caps.clone, caps.compress) {
        (true, true) => None,
        (true, false) => {
            Some("this filesystem has no transparent compression: compress finds nothing here")
        }
        (false, true) => Some(
            "this filesystem shares no blocks: dedupe links cargo home sources, artifacts only with --link-artifacts",
        ),
        (false, false) => Some(
            "this filesystem neither shares blocks nor compresses: compress finds nothing here, dedupe only links cargo home sources",
        ),
    }
}

fn status(json: bool, home: Option<PathBuf>, roots: Vec<PathBuf>) -> Result<()> {
    // The same `roots` key `run` uses; `.` stays the fallback when there is none.
    let roots = roots_or_config(roots)?;
    // Read-only and stateless: no index, so a missing `$HOME` is no reason to stop.
    let session = Session::open(Settings::default());
    let inventory = session.inventory(&roots, home_flag(home).as_deref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&inventory)?);
        return Ok(());
    }
    print_inventory(&inventory);
    Ok(())
}

fn print_inventory(inventory: &Inventory) {
    let now = session::now_unix();
    let mut family = None;
    for target in &inventory.targets {
        if family != Some(&target.family) {
            family = Some(&target.family);
            match &target.family {
                Some(dir) => println!("family {}", dir.display()),
                None => println!("no family"),
            }
        }
        let built = target.last_built_unix.map_or("never built".into(), |at| {
            format!("built {}d ago", now.saturating_sub(at) / SECS_PER_DAY)
        });
        let orphaned = if target.orphaned { "  ORPHANED" } else { "" };
        println!(
            "  {:>11}  {built:<16}{orphaned}  {}",
            gib(target.allocated_bytes),
            target.root.display()
        );
        // Only worth a line when something is missing: a filesystem that does both is the
        // case the numbers above already assume.
        if let Some(missing) = missing_caps(&target.caps) {
            println!("  {:>11}  {missing}", "");
        }
        if target.stale_units > 0 {
            let units: usize = target.toolchains.iter().map(|built| built.units).sum();
            println!(
                "  {:>11}  {} of {units} units built by an older rustc ({} toolchains)",
                format!("~{}", gib(target.stale_bytes_estimate)),
                target.stale_units,
                target.toolchains.len()
            );
        }
    }
    let sum = |field: fn(&Target) -> u64| inventory.targets.iter().map(field).sum::<u64>();
    println!(
        "{} targets, {} on disk ({} logical)",
        inventory.targets.len(),
        gib(sum(|t| t.allocated_bytes)),
        gib(sum(|t| t.logical_bytes))
    );
    if let Some(home) = &inventory.cargo_home {
        println!(
            "cargo home {}: {} on disk, not compressed yet: {}",
            home.home.display(),
            gib(home.allocated_bytes),
            gib(home.compressible_bytes)
        );
    }
    println!(
        "orphaned worktrees: {}; not compressed yet: {}; dedupe candidates (upper bound): {}",
        gib(sum(|t| if t.orphaned { t.allocated_bytes } else { 0 })),
        gib(sum(|t| t.compressible_bytes)),
        gib(sum(|t| t.dedupe_candidate_bytes))
    );
}

/// `dunnage advise`: how to print what the session found.
fn advise(json: bool, roots: Vec<PathBuf>) -> Result<()> {
    let roots = roots_or_config(roots)?;
    let session = Session::open(Settings::default());
    let advice = session.advise(&roots, cargo_home::path(None).as_deref())?;
    for warning in &advice.warnings {
        eprintln!("warning: {warning}");
    }
    let (findings, notes) = (&advice.findings, &advice.notes);
    if json {
        let report = serde_json::json!({ "findings": findings, "notes": notes });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let mut file = None;
    for finding in findings {
        if file != Some(&finding.file) {
            file = Some(&finding.file);
            println!("{}", finding.file.display());
        }
        println!("  {}: {}", finding.key, finding.note);
    }
    if !notes.is_empty() {
        println!("from the inventory");
        for note in notes {
            println!("  {}: {}", note.about, note.note);
        }
    }
    if findings.is_empty() && notes.is_empty() {
        println!("nothing to change");
    }
    Ok(())
}

/// `dunnage seed`: the session does the copy; this prints it.
fn seed_into(
    from: Option<PathBuf>,
    dry_run: bool,
    index: Option<PathBuf>,
    dir: Option<PathBuf>,
) -> Result<Done> {
    let checkout = dir.unwrap_or_else(|| PathBuf::from("."));
    let session = open(index, &load_config(None)?)?;
    let done = session.seed(&checkout, from.as_deref(), dry_run)?;
    print_seeding(&done, dry_run);
    Ok(Done::busy_if(!done.seeded.busy.is_empty()))
}

/// `dunnage worktree add`: the session adds and seeds; this says what came of each.
fn worktree_add(
    dry_run: bool,
    index: Option<PathBuf>,
    git_args: &[std::ffi::OsString],
) -> Result<Done> {
    let session = open(index, &load_config(None)?)?;
    let added = session.worktree_add(Path::new("."), git_args, dry_run)?;
    println!("added worktree {}", added.worktree.display());
    let seeding = added.seeding.with_context(|| {
        format!(
            "worktree {} is there, but seeding it failed",
            added.worktree.display()
        )
    })?;
    let Some(done) = seeding else {
        println!("  nothing to seed from: no other checkout of this repository has a target there");
        return Ok(Done::Everything);
    };
    print_seeding(&done, dry_run);
    Ok(Done::busy_if(!done.seeded.busy.is_empty()))
}

fn print_seeding(done: &session::Seeding, dry_run: bool) {
    let seeded = &done.seeded;
    let verb = if dry_run { "would copy" } else { "copied" };
    println!(
        "{} from {}: {verb} {} files and {} symlinks, {} that the clones share with it",
        done.target.display(),
        seeded.source.display(),
        seeded.files,
        seeded.symlinks,
        gib(seeded.bytes)
    );
    for dir in &seeded.busy {
        println!("  busy, not copied: {}", dir.display());
    }
}

/// Flags over the config file: a flag always wins.
fn request(args: RunArgs, config: &Config) -> Request {
    let mut request = Request::from_config(config);
    if !args.lossy.is_empty() {
        request.lossy = args.lossy;
    }
    request.passes = args.pass;
    if let Some(days) = args.evict_idle_days {
        request.evict.idle_days = Some(days);
    }
    if let Some(gib) = args.evict_max_total_gib {
        request.evict.max_total_bytes = Some(session::gib_to_bytes(gib));
    }
    request.evict_whole_target |= args.evict_whole_target;
    if let Some(days) = args.incremental_idle_days {
        request.incremental_idle_days = Some(days);
    }
    if let Some(secs) = args.min_age {
        request.min_age = Some(Duration::from_secs(secs));
    }
    if let Some(bytes) = args.min_size {
        request.min_size = Some(bytes);
    }
    request.cargo_home = home_flag(args.cargo_home);
    if !args.store.is_empty() {
        request.stores = args.store;
    }
    request.across_families |= args.across_families;
    request.link_artifacts = args.link_artifacts;
    request.until_settled = true;
    if !args.roots.is_empty() {
        request.roots = args.roots;
    }
    request
}

fn run(args: RunArgs) -> Result<Done> {
    let config = load_config(args.config.as_deref())?;
    let (dry_run, json, index) = (args.dry_run, args.json, args.index.clone());
    let request = request(args, &config);
    // Before anything else can fail, so a mistyped flag is named even without a `$HOME`.
    request.check()?;
    let session = open(index, &config)?;
    let table = Table {
        link_warning: Cell::new(request.link_artifacts),
        dry_run,
    };
    let quiet = session::Quiet;
    let control = Control {
        observer: if json { &quiet } else { &table },
        ..Control::default()
    };
    let report = if dry_run {
        session.plan(&request, &control)?
    } else {
        session.apply(&request, &control)?
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&JsonReport::new(&report))?
        );
    } else {
        for note in &report.compress_notes {
            println!("compress backend: {note}");
        }
        println!("files hashed: {}", report.files_hashed);
    }
    Ok(Done::busy_if(report.left_busy))
}

/// The table a run prints as it goes.
struct Table {
    /// Still to be said, once, before the first group.
    link_warning: Cell<bool>,
    dry_run: bool,
}

impl Observer for Table {
    fn group(&self, group: &Path) {
        if self.link_warning.replace(false) {
            eprintln!(
                "--link-artifacts: equal artifacts may become one inode where the filesystem \
                 cannot share blocks. A build that rewrites one of them rewrites the others."
            );
        }
        println!("{}", group.display());
    }

    fn report(&self, _group: &Path, report: &engine::Report) {
        print_report(report, self.dry_run);
    }
}

fn print_report(report: &engine::Report, dry_run: bool) {
    for dir in &report.busy {
        println!("  busy, skipped: {}", dir.display());
    }
    for dir in &report.quiet {
        println!("  no build lock, weaker checks: {}", dir.display());
    }
    if report.temps_removed > 0 {
        println!("  stale temp files removed: {}", report.temps_removed);
    }
    for pass in &report.passes {
        println!(
            "  {}: planned {} ({} bytes), applied {} ({} bytes), skipped {}",
            pass.name,
            pass.planned,
            pass.planned_bytes,
            pass.applied,
            pass.freed_bytes,
            pass.skipped.len()
        );
        let verb = if dry_run { "would remove" } else { "remove" };
        for (dir, reason) in &pass.removals {
            println!("    {verb} {}: {reason}", dir.display());
        }
        for (path, skip) in &pass.skipped {
            println!("    skipped {}: {skip:?}", path.display());
        }
    }
}

/// `--json`: the same report as the table, for a script that has to act on it.
#[derive(serde::Serialize)]
struct JsonReport<'a> {
    dry_run: bool,
    groups: Vec<JsonGroup<'a>>,
    compress_notes: &'a [String],
    files_hashed: usize,
}

impl<'a> JsonReport<'a> {
    fn new(report: &'a RunReport) -> Self {
        Self {
            dry_run: report.dry_run,
            groups: report
                .groups
                .iter()
                .map(|(group, report)| JsonGroup::new(group, report))
                .collect(),
            compress_notes: &report.compress_notes,
            files_hashed: report.files_hashed,
        }
    }
}

#[derive(serde::Serialize)]
struct JsonGroup<'a> {
    family: &'a Path,
    busy: &'a [PathBuf],
    /// Worked on without a lock: `DESIGN.md`, "Safety tier without a build lock".
    quiet: &'a [PathBuf],
    temps_removed: usize,
    passes: Vec<JsonPass<'a>>,
}

impl<'a> JsonGroup<'a> {
    fn new(family: &'a Path, report: &'a engine::Report) -> Self {
        Self {
            family,
            busy: &report.busy,
            quiet: &report.quiet,
            temps_removed: report.temps_removed,
            passes: report.passes.iter().map(JsonPass::new).collect(),
        }
    }
}

#[derive(serde::Serialize)]
struct JsonPass<'a> {
    name: &'a str,
    planned: usize,
    planned_bytes: u64,
    applied: usize,
    freed_bytes: u64,
    removals: Vec<JsonNote<'a>>,
    skipped: Vec<JsonNote<'a>>,
}

impl<'a> JsonPass<'a> {
    fn new(pass: &'a engine::PassReport) -> Self {
        let removals = pass.removals.iter().map(|(path, reason)| JsonNote {
            path,
            reason: reason.clone(),
        });
        let skipped = pass.skipped.iter().map(|(path, skip)| JsonNote {
            path,
            reason: format!("{skip:?}"),
        });
        Self {
            name: pass.name,
            planned: pass.planned,
            planned_bytes: pass.planned_bytes,
            applied: pass.applied,
            freed_bytes: pass.freed_bytes,
            removals: removals.collect(),
            skipped: skipped.collect(),
        }
    }
}

#[derive(serde::Serialize)]
struct JsonNote<'a> {
    path: &'a Path,
    reason: String,
}
