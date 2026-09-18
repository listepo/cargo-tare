use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use cargo_tare::advise::{self, Kind};
use cargo_tare::cargo_home;
use cargo_tare::compress::{self, Compress};
use cargo_tare::config::{self, Config};
use cargo_tare::dedupe::{self, Dedupe};
use cargo_tare::doc::{self, Doc, Docs};
use cargo_tare::engine::{self, Locks, Options, Pass};
use cargo_tare::evict::{self, Evict, Limits};
use cargo_tare::incremental::{self, Incremental};
use cargo_tare::index::HashIndex;
use cargo_tare::inventory::{self, Inventory, ProfileInfo, Target};
use cargo_tare::orphans::{self, Orphan, Orphans};
use cargo_tare::seed;
use clap::{Parser, Subcommand};

/// Relative to `$HOME`. The digit follows the index file format.
const DEFAULT_INDEX: &str = ".cache/cargo-tare/hashes-v1.bin";
const BYTES_PER_GIB: f64 = (1u64 << 30) as f64;
/// Exit code when a profile dir was skipped because a build holds its lock.
const BUSY_EXIT: u8 = 2;
/// Every pass that deletes rebuildable data; `--lossy` takes these names.
/// What the report calls the one group `--across-families` makes, in place of a family dir.
const ACROSS_FAMILIES: &str = "<across families>";
const LOSSY_PASSES: [&str; 4] = [orphans::NAME, evict::NAME, incremental::NAME, doc::NAME];
/// Every pass, in pipeline order; `--pass` takes these names.
const PASSES: [&str; 6] = [
    orphans::NAME,
    evict::NAME,
    incremental::NAME,
    doc::NAME,
    compress::NAME,
    dedupe::NAME,
];
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// Cargo runs external subcommands as `cargo-tare tare <args>`.
#[derive(Parser)]
#[command(name = "cargo", bin_name = "cargo")]
enum Cargo {
    Tare(Tare),
}

/// Shrink Cargo target directories without slowing builds
#[derive(clap::Args)]
#[command(version)]
struct Tare {
    #[command(subcommand)]
    cmd: Cmd,
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
    /// Clone a sibling checkout's target into a fresh one, so its first build starts warm
    Seed {
        /// Where to copy from: a checkout or a target dir [default: the family's newest target]
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
        /// Report what would be copied without touching anything
        #[arg(long)]
        dry_run: bool,
        /// Content-hash cache [default: ~/.cache/cargo-tare/hashes-v1.bin]
        #[arg(long, value_name = "FILE")]
        index: Option<PathBuf>,
        /// The checkout to seed [default: .]
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
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
    /// Content-hash cache [default: ~/.cache/cargo-tare/hashes-v1.bin]
    #[arg(long, value_name = "FILE")]
    index: Option<PathBuf>,
    /// Configuration file [default: $XDG_CONFIG_HOME/cargo-tare/config.toml]
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
    let Cargo::Tare(Tare { cmd }) = Cargo::parse();
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
    };
    match done {
        Ok(Done::Everything) => ExitCode::SUCCESS,
        // A cron job wants to tell "nothing to do" from "a build was in the way".
        Ok(Done::LeftBusy) => ExitCode::from(BUSY_EXIT),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// What `run` finished with; the difference is visible in the exit code.
enum Done {
    Everything,
    LeftBusy,
}

/// Canonical, so that families, scanned paths and locked dirs all compare equal.
fn read_inventory(mut roots: Vec<PathBuf>) -> Result<Inventory> {
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    for root in &mut roots {
        *root = root
            .canonicalize()
            .with_context(|| format!("{}", root.display()))?;
    }
    Ok(inventory::inventory(&roots)?)
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / BYTES_PER_GIB)
}

/// What the filesystem under a target cannot do, in the words of the passes it silences.
/// `None` when it can do everything, which needs no line.
fn missing_caps(caps: &cargo_tare::sys::Caps) -> Option<&'static str> {
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

fn status(json: bool, home: Option<PathBuf>, mut roots: Vec<PathBuf>) -> Result<()> {
    if roots.is_empty() {
        // The same `roots` key `run` uses; `.` stays the fallback when there is none.
        roots = match config::default_path() {
            Some(path) => Config::load(&path)?.roots,
            None => Vec::new(),
        };
    }
    let mut inventory = read_inventory(roots)?;
    if let Some(home) =
        home.and_then(|flag| cargo_home::path((!flag.as_os_str().is_empty()).then_some(flag)))
    {
        inventory.cargo_home = Some(cargo_home::inspect(&home)?);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&inventory)?);
        return Ok(());
    }
    let now = now_unix();
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
    Ok(())
}

/// `cargo tare advise`: which files to read and how to print what they say. The checks live in
/// `advise.rs`; this reads nothing but text and writes nothing at all.
fn advise(json: bool, mut roots: Vec<PathBuf>) -> Result<()> {
    if roots.is_empty() {
        roots = match config::default_path() {
            Some(path) => Config::load(&path)?.roots,
            None => Vec::new(),
        };
    }
    let inventory = read_inventory(roots)?;
    let mut findings = Vec::new();
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
            findings.extend(review_file(&file, kind, project));
            read.push(file);
        }
    }
    // The cargo home is advised on even when it has no config file at all: the keys it is
    // missing are the point.
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    if let Some(home) = home {
        findings.extend(review_file(&home.join("config.toml"), Kind::Home, &home));
    }
    let notes = advise::notes(&inventory.targets);
    if json {
        let report = serde_json::json!({ "findings": findings, "notes": notes });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let mut file = None;
    for finding in &findings {
        if file != Some(&finding.file) {
            file = Some(&finding.file);
            println!("{}", finding.file.display());
        }
        println!("  {}: {}", finding.key, finding.note);
    }
    if !notes.is_empty() {
        println!("from the inventory");
        for note in &notes {
            println!("  {}: {}", note.about, note.note);
        }
    }
    if findings.is_empty() && notes.is_empty() {
        println!("nothing to change");
    }
    Ok(())
}

/// One file's findings. A file that is not there is not a finding of its own — except for the
/// cargo home's config, whose absent keys `review` reports from an empty document.
fn review_file(file: &Path, kind: Kind, dir: &Path) -> Vec<advise::Finding> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(_) if kind == Kind::Home => String::new(),
        Err(_) => return Vec::new(),
    };
    let doc: toml::Table = match text.parse() {
        Ok(doc) => doc,
        Err(error) => {
            eprintln!("warning: {}: {error}", file.display());
            return Vec::new();
        }
    };
    // Only asked when the answer matters, since it costs a process.
    let nightly = doc.get("unstable").is_some() && nightly_toolchain(dir);
    advise::review(file, kind, &doc, nightly)
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

fn index_path(flag: Option<PathBuf>) -> Result<PathBuf> {
    match flag {
        Some(path) => Ok(path),
        None => {
            let home = std::env::var_os("HOME").context("HOME is not set; pass --index")?;
            Ok(PathBuf::from(home).join(DEFAULT_INDEX))
        }
    }
}

/// `cargo tare seed`: the whole command, since the copy itself lives in `seed.rs`.
fn seed_into(
    from: Option<PathBuf>,
    dry_run: bool,
    index: Option<PathBuf>,
    dir: Option<PathBuf>,
) -> Result<Done> {
    let checkout = dir.unwrap_or_else(|| PathBuf::from("."));
    let checkout = checkout
        .canonicalize()
        .with_context(|| format!("{}", checkout.display()))?;
    let source = match from {
        Some(path) => {
            let path = path
                .canonicalize()
                .with_context(|| format!("{}", path.display()))?;
            // A checkout or its target dir; both are what somebody means by "from there".
            let target = path.join(seed::TARGET);
            if target.is_dir() { target } else { path }
        }
        None => seed::choose(&checkout).context(
            "no other checkout of this repository has a target dir; name one with --from",
        )?,
    };
    let index_path = index_path(index)?;
    let mut hashes = HashIndex::load(&index_path);
    let seeded = seed::seed(&checkout, &source, &mut hashes, dry_run)
        .with_context(|| format!("seeding {} from {}", checkout.display(), source.display()))?;
    let verb = if dry_run { "would copy" } else { "copied" };
    println!(
        "{} from {}: {verb} {} files and {} symlinks, {} that the clones share with it",
        checkout.join(seed::TARGET).display(),
        seeded.source.display(),
        seeded.files,
        seeded.symlinks,
        gib(seeded.bytes)
    );
    for dir in &seeded.busy {
        println!("  busy, not copied: {}", dir.display());
    }
    if !dry_run {
        hashes
            .save(&index_path)
            .with_context(|| format!("saving {}", index_path.display()))?;
    }
    Ok(if seeded.busy.is_empty() {
        Done::Everything
    } else {
        Done::LeftBusy
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_secs())
}

fn run(args: RunArgs) -> Result<Done> {
    let config = match &args.config {
        // A file the command line names and that is not there is a mistake, not a default.
        Some(path) => {
            ensure!(path.exists(), "no config file at {}", path.display());
            Config::load(path)?
        }
        None => match config::default_path() {
            Some(path) => Config::load(&path)?,
            None => Config::default(),
        },
    };
    let opts = Options {
        dry_run: args.dry_run,
        lossy: if args.lossy.is_empty() {
            config.lossy.clone()
        } else {
            args.lossy
        },
    };
    for name in &opts.lossy {
        ensure!(
            LOSSY_PASSES.contains(&name.as_str()),
            "unknown lossy pass `{name}`"
        );
    }
    for name in &args.pass {
        ensure!(PASSES.contains(&name.as_str()), "unknown pass `{name}`");
    }
    let limits = Limits {
        idle_days: args.evict_idle_days.or(config.evict.idle_days),
        max_total_bytes: args
            .evict_max_total_gib
            .or(config.evict.max_total_gib)
            .map(|gib| gib.saturating_mul(1 << 30)),
    };
    let evicting = opts.lossy.iter().any(|name| name == evict::NAME);
    ensure!(
        evicting != (limits == Limits::default()),
        "`--lossy evict` and a limit (--evict-idle-days, --evict-max-total-gib) need each other"
    );
    let incremental_idle_days = args.incremental_idle_days.or(config.incremental.idle_days);
    let dropping = opts.lossy.iter().any(|name| name == incremental::NAME);
    ensure!(
        dropping == incremental_idle_days.is_some(),
        "`--lossy incremental` and `--incremental-idle-days` need each other"
    );
    let index_path = index_path(args.index)?;
    let index = RefCell::new(HashIndex::load(&index_path));
    let (mut compress, mut dedupe) = (Compress::new(&index), Dedupe::new(&index));
    // Artifacts are only linked when the user asks; the cargo home's unpacked sources are
    // always safe to link, because cargo replaces a source dir instead of rewriting its files.
    let mut home_dedupe = Dedupe::new(&index);
    dedupe.link_fallback = args.link_artifacts;
    home_dedupe.link_fallback = true;
    if let Some(secs) = args.min_age.or(config.min_age) {
        let min_age = Duration::from_secs(secs);
        (compress.min_age, dedupe.min_age) = (min_age, min_age);
        home_dedupe.min_age = min_age;
    }
    if let Some(bytes) = args.min_size.or(config.min_size) {
        (compress.min_size, dedupe.min_size) = (bytes, bytes);
        home_dedupe.min_size = bytes;
    }

    // One engine run per family: its locks block builds only in the targets being compared.
    // ponytail: equal files in unrelated projects (the same registry crates) are not shared;
    // run families together if the benchmarks say it is worth the wider lock.
    let roots = if args.roots.is_empty() {
        config.roots.clone()
    } else {
        args.roots
    };
    // `--cargo-home` is a run of its own: it needs no target and no root.
    let only_home = roots.is_empty() && args.cargo_home.is_some();
    ensure!(
        !roots.is_empty() || only_home,
        "no roots: name them on the command line or set `roots` in the config file"
    );
    let inventory = if only_home {
        Inventory::default()
    } else {
        read_inventory(roots)?
    };
    ensure!(
        !inventory.targets.is_empty() || only_home,
        "no cargo target dirs found"
    );
    // The size cap is global, so eviction is decided over everything under the roots at once.
    let profiles: Vec<ProfileInfo> = inventory
        .targets
        .iter()
        .flat_map(|target| target.profiles.iter().cloned())
        .collect();
    let chosen = evict::select(&profiles, now_unix(), limits);
    let mut evict = Evict::new(chosen.clone());
    if args.evict_whole_target || config.evict.whole_target {
        evict = evict.whole(evict::whole_targets(&inventory.targets, &chosen));
    }
    // Cargo keeps `incremental/` for workspace members only, so this costs one plain rebuild.
    let idle_days = incremental_idle_days.unwrap_or(u64::MAX);
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
        .filter(|pass| args.pass.is_empty() || args.pass.iter().any(|name| name == pass.name()))
        .collect();
    // One group per family keeps a run's locks inside the repository it is working on. Across
    // families every target is compared with every other — unrelated projects do share
    // artifacts — and the price is that the locks of all of them are held for the whole run.
    let across = args.across_families || config.across_families;
    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for target in inventory.targets {
        let family = target.family.unwrap_or_else(|| target.root.clone());
        if config.skips(&family) {
            continue;
        }
        // Not a path: the group is every family at once, and the report says so.
        let key = if across {
            PathBuf::from(ACROSS_FAMILIES)
        } else {
            family
        };
        let dirs = target.profiles.into_iter().map(|profile| profile.dir);
        groups.entry(key).or_default().extend(dirs);
    }

    if args.link_artifacts && !args.json {
        eprintln!(
            "--link-artifacts: equal artifacts may become one inode where the filesystem \
             cannot share blocks. A build that rewrites one of them rewrites the others."
        );
    }
    let mut left_busy = false;
    let mut reports = Vec::new();
    for (group, profile_dirs) in &groups {
        if !args.json {
            println!("{}", group.display());
        }
        let report = engine::run(profile_dirs, &passes, &opts, Locks::PerDir)?;
        left_busy |= !report.busy.is_empty();
        if args.json {
            reports.push((group.as_path(), report));
        } else {
            print_report(&report, opts.dry_run);
        }
    }
    // One more group, guarded by cargo's own home lock instead of per-profile locks. Only
    // `compress` runs here: these are unpacked sources, not build output.
    let home = args
        .cargo_home
        .and_then(|flag| cargo_home::path((!flag.as_os_str().is_empty()).then_some(flag)));
    if let Some(home) = &home {
        let dirs = cargo_home::dirs(home);
        ensure!(
            !dirs.is_empty(),
            "no {} or {} in {}",
            cargo_home::DIRS[0],
            cargo_home::DIRS[1],
            home.display()
        );
        let lock = home.join(cargo_home::LOCK_FILE);
        ensure!(
            lock.is_file(),
            "no {} in {}: cargo has never used it as its home",
            cargo_home::LOCK_FILE,
            home.display()
        );
        if !args.json {
            println!("{}", home.display());
        }
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
        let report = engine::run(&dirs, &home_passes, &opts, Locks::Shared(&lock))?;
        left_busy |= !report.busy.is_empty();
        if args.json {
            reports.push((home.as_path(), report));
        } else {
            print_report(&report, opts.dry_run);
        }
    }
    if args.json {
        let json = JsonReport {
            dry_run: opts.dry_run,
            groups: reports
                .iter()
                .map(|(group, report)| JsonGroup::new(group, report))
                .collect(),
            compress_notes: compress.notes(),
            files_hashed: dedupe.hashed(),
        };
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        for note in compress.notes() {
            println!("compress backend: {note}");
        }
        println!("files hashed: {}", dedupe.hashed());
    }
    // The index only caches hashes of files as they are, so it is worth keeping on a dry run too.
    index
        .borrow()
        .save(&index_path)
        .with_context(|| format!("saving {}", index_path.display()))?;
    Ok(if left_busy {
        Done::LeftBusy
    } else {
        Done::Everything
    })
}

fn print_report(report: &engine::Report, dry_run: bool) {
    for dir in &report.busy {
        println!("  busy, skipped: {}", dir.display());
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
    compress_notes: Vec<String>,
    files_hashed: usize,
}

#[derive(serde::Serialize)]
struct JsonGroup<'a> {
    family: &'a Path,
    busy: &'a [PathBuf],
    temps_removed: usize,
    passes: Vec<JsonPass<'a>>,
}

impl<'a> JsonGroup<'a> {
    fn new(family: &'a Path, report: &'a engine::Report) -> Self {
        Self {
            family,
            busy: &report.busy,
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
