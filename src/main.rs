use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use cargo_tare::compress::{self, Compress};
use cargo_tare::dedupe::{self, Dedupe};
use cargo_tare::engine::{self, Options, Pass};
use cargo_tare::evict::{self, Evict, Limits};
use cargo_tare::index::HashIndex;
use cargo_tare::inventory::{self, Inventory, ProfileInfo, Target};
use cargo_tare::orphans::{self, Orphan, Orphans};
use clap::{Parser, Subcommand};

/// Relative to `$HOME`. The digit follows the index file format.
const DEFAULT_INDEX: &str = ".cache/cargo-tare/hashes-v1.bin";
const BYTES_PER_GIB: f64 = (1u64 << 30) as f64;
/// Every pass that deletes rebuildable data; `--lossy` takes these names.
const LOSSY_PASSES: [&str; 2] = [orphans::NAME, evict::NAME];
/// Every pass, in pipeline order; `--pass` takes these names.
const PASSES: [&str; 4] = [orphans::NAME, evict::NAME, compress::NAME, dedupe::NAME];
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
        /// Dirs to search; a target dir itself works too [default: .]
        #[arg(value_name = "ROOT")]
        roots: Vec<PathBuf>,
    },
    /// Plan and apply the passes, one family of targets at a time; profile dirs with a running
    /// build are skipped
    Run(RunArgs),
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
    /// Leave files younger than this alone, in seconds; both lossless passes [default: 3600]
    #[arg(long, value_name = "SECS")]
    min_age: Option<u64>,
    /// Leave files smaller than this alone; both lossless passes [default: 8192 / 4096]
    #[arg(long, value_name = "BYTES")]
    min_size: Option<u64>,
    /// Content-hash cache [default: ~/.cache/cargo-tare/hashes-v1.bin]
    #[arg(long, value_name = "FILE")]
    index: Option<PathBuf>,
    /// Dirs to search; a target dir itself works too
    #[arg(required = true, value_name = "ROOT")]
    roots: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let Cargo::Tare(Tare { cmd }) = Cargo::parse();
    match cmd {
        Cmd::Status { json, roots } => status(json, roots),
        Cmd::Run(args) => run(args),
    }
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

fn status(json: bool, roots: Vec<PathBuf>) -> Result<()> {
    let inventory = read_inventory(roots)?;
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
    }
    let sum = |field: fn(&Target) -> u64| inventory.targets.iter().map(field).sum::<u64>();
    println!(
        "{} targets, {} on disk ({} logical)",
        inventory.targets.len(),
        gib(sum(|t| t.allocated_bytes)),
        gib(sum(|t| t.logical_bytes))
    );
    println!(
        "orphaned worktrees: {}; not compressed yet: {}; dedupe candidates (upper bound): {}",
        gib(sum(|t| if t.orphaned { t.allocated_bytes } else { 0 })),
        gib(sum(|t| t.compressible_bytes)),
        gib(sum(|t| t.dedupe_candidate_bytes))
    );
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_secs())
}

fn run(args: RunArgs) -> Result<()> {
    let opts = Options {
        dry_run: args.dry_run,
        lossy: args.lossy,
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
        idle_days: args.evict_idle_days,
        max_total_bytes: args
            .evict_max_total_gib
            .map(|gib| gib.saturating_mul(1 << 30)),
    };
    let evicting = opts.lossy.iter().any(|name| name == evict::NAME);
    ensure!(
        evicting != (limits == Limits::default()),
        "`--lossy evict` and a limit (--evict-idle-days, --evict-max-total-gib) need each other"
    );
    let index_path = match args.index {
        Some(path) => path,
        None => {
            let home = std::env::var_os("HOME").context("HOME is not set; pass --index")?;
            PathBuf::from(home).join(DEFAULT_INDEX)
        }
    };
    let index = RefCell::new(HashIndex::load(&index_path));
    let (mut compress, mut dedupe) = (Compress::new(&index), Dedupe::new(&index));
    if let Some(secs) = args.min_age {
        let min_age = Duration::from_secs(secs);
        (compress.min_age, dedupe.min_age) = (min_age, min_age);
    }
    if let Some(bytes) = args.min_size {
        (compress.min_size, dedupe.min_size) = (bytes, bytes);
    }

    // One engine run per family: its locks block builds only in the targets being compared.
    // ponytail: equal files in unrelated projects (the same registry crates) are not shared;
    // run families together if the benchmarks say it is worth the wider lock.
    let inventory = read_inventory(args.roots)?;
    ensure!(!inventory.targets.is_empty(), "no cargo target dirs found");
    // The size cap is global, so eviction is decided over everything under the roots at once.
    let profiles: Vec<ProfileInfo> = inventory
        .targets
        .iter()
        .flat_map(|target| target.profiles.iter().cloned())
        .collect();
    let evict = Evict::new(evict::select(&profiles, now_unix(), limits));
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
    // Pipeline order (`DESIGN.md`): orphans, evict, compress, dedupe.
    let all: [&dyn Pass; 4] = [&orphans, &evict, &compress, &dedupe];
    let passes: Vec<&dyn Pass> = all
        .into_iter()
        .filter(|pass| args.pass.is_empty() || args.pass.iter().any(|name| name == pass.name()))
        .collect();
    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for target in inventory.targets {
        let key = target.family.unwrap_or_else(|| target.root.clone());
        let dirs = target.profiles.into_iter().map(|profile| profile.dir);
        groups.entry(key).or_default().extend(dirs);
    }

    for (group, profile_dirs) in &groups {
        println!("{}", group.display());
        let report = engine::run(profile_dirs, &passes, &opts)?;
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
            let verb = if opts.dry_run {
                "would remove"
            } else {
                "remove"
            };
            for (dir, reason) in &pass.removals {
                println!("    {verb} {}: {reason}", dir.display());
            }
            for (path, skip) in &pass.skipped {
                println!("    skipped {}: {skip:?}", path.display());
            }
        }
    }
    for note in compress.notes() {
        println!("compress backend: {note}");
    }
    println!("files hashed: {}", dedupe.hashed());
    // The index only caches hashes of files as they are, so it is worth keeping on a dry run too.
    index
        .borrow()
        .save(&index_path)
        .with_context(|| format!("saving {}", index_path.display()))?;
    Ok(())
}
