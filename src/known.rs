//! The build dirs a walk found, kept next to the hash index so that the next run can skip the
//! walk. In a monorepo the walk costs the source tree, not the build dirs: `docs/bench.md`,
//! "Discovery in a monorepo".
//!
//! The list holds while the roots are the same, it is younger than the cadence, and no root's own
//! mtime moved. Every entry is checked again by its adapter's `claim`, so a removed build dir
//! drops out without a walk; a new one waits for the next walk.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::eco::{self, Ecosystem};

/// Next to the hash index.
pub const FILE: &str = "build-dirs-v1.json";
/// How long a walk holds when the config says nothing.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(60 * 60);

/// Build dirs with the adapter that claimed each, as [`eco::discover`] returns them.
pub type Found = Vec<(PathBuf, &'static dyn Ecosystem)>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Known {
    roots: Vec<PathBuf>,
    walked_unix: u64,
    /// Each root's mtime in nanoseconds, in the order of `roots`.
    root_mtimes: Vec<Option<u64>>,
    /// Each build dir with its adapter's name.
    dirs: Vec<(PathBuf, String)>,
}

fn mtime(path: &Path) -> Option<u64> {
    let since = fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    u64::try_from(since.as_nanos()).ok()
}

/// The build dirs under `roots`, and whether the roots were walked for them. The list in `file`
/// is used unless there is none, `walk` asks for a walk, `every` is zero, or the list no longer
/// holds; a walk is written back as the new list.
pub fn discover(
    file: Option<&Path>,
    roots: &[PathBuf],
    every: Duration,
    walk: bool,
    now: u64,
) -> (Found, bool) {
    // No roots is no walk to save: a run of only `--store` or `--cargo-home` groups.
    let Some(file) = file.filter(|_| !roots.is_empty()) else {
        return (eco::discover(roots), true);
    };
    if !walk
        && !every.is_zero()
        && let Some(found) = load(file, roots, every, now)
    {
        return (found, false);
    }
    // Read before the walk: a root that changes while it runs must not look walked.
    let mtimes = roots.iter().map(|root| mtime(root)).collect();
    let found = eco::discover(roots);
    // A list that cannot be written costs the next run a walk, nothing more.
    let _ = save(file, roots, mtimes, &found, now);
    (found, true)
}

fn load(file: &Path, roots: &[PathBuf], every: Duration, now: u64) -> Option<Found> {
    let known: Known = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    let mtimes: Vec<Option<u64>> = roots.iter().map(|root| mtime(root)).collect();
    let holds = known.roots == roots
        && now.saturating_sub(known.walked_unix) < every.as_secs()
        && known.root_mtimes == mtimes;
    if !holds {
        return None;
    }
    let found = known
        .dirs
        .into_iter()
        .filter_map(|(dir, name)| {
            let eco = eco::named(&name)?;
            eco.claim(&dir).then_some((dir, eco))
        })
        .collect();
    Some(found)
}

/// Written whole and renamed into place, so a run never reads half a list.
fn save(
    file: &Path,
    roots: &[PathBuf],
    root_mtimes: Vec<Option<u64>>,
    found: &Found,
    now: u64,
) -> io::Result<()> {
    let known = Known {
        roots: roots.to_vec(),
        walked_unix: now,
        root_mtimes,
        dirs: found
            .iter()
            .map(|(dir, eco)| (dir.clone(), eco.name().to_owned()))
            .collect(),
    };
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = file.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec(&known)?)?;
    fs::rename(&temp, file)
}
