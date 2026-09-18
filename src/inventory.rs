//! Read-only inventory: which cargo targets exist under some roots, how big they really are,
//! which belong together, and what the passes could win. Takes no locks and changes nothing.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use walkdir::WalkDir;

use crate::compress::DEFAULT_MIN_SIZE as COMPRESS_MIN_SIZE;
use crate::dedupe::DEFAULT_MIN_SIZE as DEDUPE_MIN_SIZE;
use crate::engine::UF_COMPRESSED;
use crate::model;

const GITDIR_KEY: &str = "gitdir:";
const WORKTREES_DIR: &str = "worktrees";

/// The unit the lossy `evict` pass removes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileInfo {
    pub dir: PathBuf,
    /// Inodes are counted for the profile dir that holds their first path.
    pub allocated_bytes: u64,
    /// Newest mtime among the dir's top-level entries, as unix seconds.
    pub last_built_unix: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
pub struct Target {
    pub root: PathBuf,
    pub profiles: Vec<ProfileInfo>,
    /// Targets with the same git common dir (a repository and its worktrees) form a family.
    pub family: Option<PathBuf>,
    /// The project is a git worktree whose record in the repository is gone.
    pub orphaned: bool,
    pub inodes: usize,
    pub paths: usize,
    pub logical_bytes: u64,
    /// What `du` reports: allocated blocks, every hardlinked inode once.
    pub allocated_bytes: u64,
    pub compressed_bytes: u64,
    /// Allocated bytes of files the compress pass would still look at.
    pub compressible_bytes: u64,
    /// Allocated bytes of the profiles' `incremental/` dirs, which no build needs to keep.
    pub incremental_bytes: u64,
    /// Upper bound for dedupe: bytes of files whose size also occurs in a sibling target.
    pub dedupe_candidate_bytes: u64,
    /// The latest build of any profile, as unix seconds.
    pub last_built_unix: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
pub struct Inventory {
    /// Sorted by family, then by allocated size, largest first.
    pub targets: Vec<Target>,
}

/// Cargo target and build dirs under `roots`. A found target is not entered.
pub fn discover(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        let mut walk = WalkDir::new(root).follow_links(false).into_iter();
        while let Some(entry) = walk.next() {
            // A dir we may not read cannot hold a target we could work on.
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_dir() && model::is_cargo_target(entry.path()) {
                found.push(entry.path().to_path_buf());
                walk.skip_current_dir();
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

pub fn inventory(roots: &[PathBuf]) -> io::Result<Inventory> {
    let mut targets = Vec::new();
    let mut sizes = Vec::new();
    for root in discover(roots) {
        let (target, target_sizes) = inspect(&root)?;
        targets.push(target);
        sizes.push(target_sizes);
    }

    // How many targets of a family hold a file of a given size.
    let mut holders: HashMap<(&Path, u64), usize> = HashMap::new();
    for (target, target_sizes) in targets.iter().zip(&sizes) {
        if let Some(family) = &target.family {
            for &size in target_sizes.keys() {
                *holders.entry((family, size)).or_default() += 1;
            }
        }
    }
    let candidates: Vec<u64> = targets
        .iter()
        .zip(&sizes)
        .map(|(target, target_sizes)| {
            let Some(family) = &target.family else {
                return 0;
            };
            target_sizes
                .iter()
                .filter(|&(&size, _)| holders[&(family.as_path(), size)] > 1)
                .map(|(_, allocated)| allocated)
                .sum()
        })
        .collect();
    for (target, bytes) in targets.iter_mut().zip(candidates) {
        target.dedupe_candidate_bytes = bytes;
    }

    targets.sort_by(|a, b| {
        (&a.family, b.allocated_bytes, &a.root).cmp(&(&b.family, a.allocated_bytes, &b.root))
    });
    Ok(Inventory { targets })
}

/// Totals of one target, plus allocated bytes per file size for the dedupe estimate.
fn inspect(root: &Path) -> io::Result<(Target, HashMap<u64, u64>)> {
    let profiles: Vec<ProfileInfo> = model::profile_dirs(root)?
        .into_iter()
        .map(|dir| ProfileInfo {
            last_built_unix: last_built(&dir),
            allocated_bytes: 0,
            dir,
        })
        .collect();
    let (family, orphaned) = git_link(root);
    let mut target = Target {
        root: root.to_path_buf(),
        last_built_unix: profiles.iter().filter_map(|p| p.last_built_unix).max(),
        profiles,
        family,
        orphaned,
        inodes: 0,
        paths: 0,
        logical_bytes: 0,
        allocated_bytes: 0,
        compressed_bytes: 0,
        compressible_bytes: 0,
        incremental_bytes: 0,
        dedupe_candidate_bytes: 0,
    };
    let mut sizes: HashMap<u64, u64> = HashMap::new();
    // The whole target, not only the profile dirs: `doc/`, `package/` and `tmp/` weigh too.
    for inode in model::scan(root)?.inodes {
        target.inodes += 1;
        target.paths += inode.paths.len();
        target.logical_bytes += inode.stamp.size;
        target.allocated_bytes += inode.allocated;
        let holder = target
            .profiles
            .iter_mut()
            .find(|profile| inode.paths[0].starts_with(&profile.dir));
        if let Some(profile) = holder {
            profile.allocated_bytes += inode.allocated;
            if inode.paths[0].starts_with(profile.dir.join(crate::incremental::DIR)) {
                target.incremental_bytes += inode.allocated;
            }
        }
        if inode.flags & UF_COMPRESSED != 0 {
            target.compressed_bytes += inode.allocated;
        } else if inode.stamp.size >= COMPRESS_MIN_SIZE {
            target.compressible_bytes += inode.allocated;
        }
        if inode.stamp.size >= DEDUPE_MIN_SIZE {
            *sizes.entry(inode.stamp.size).or_default() += inode.allocated;
        }
    }
    Ok((target, sizes))
}

/// When cargo last worked in a profile dir: the newest mtime among its top-level entries, as
/// unix seconds.
pub fn last_built(profile_dir: &Path) -> Option<u64> {
    fs::read_dir(profile_dir)
        .ok()?
        .filter_map(|entry| entry.ok()?.metadata().ok()?.modified().ok())
        .max()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|since_epoch| since_epoch.as_secs())
}

/// Whether the project that owns `target` is a git worktree its repository no longer knows.
/// Cheap enough to repeat under the lock, which is what the `orphans` pass does.
pub fn is_orphaned(target: &Path) -> bool {
    git_link(target).1
}

/// The git common dir of the project that owns `target`, if it has one. `target` need not
/// exist: only the directories above it are read.
pub fn family(target: &Path) -> Option<PathBuf> {
    git_link(target).0
}

/// Every checkout git registers under this common dir: the repository itself and each worktree.
/// Read from git's files directly, as everything else here is.
pub fn checkouts(common: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = common.parent().map(Path::to_path_buf).into_iter().collect();
    let entries = fs::read_dir(common.join(WORKTREES_DIR))
        .into_iter()
        .flatten();
    for entry in entries.flatten() {
        // `<common>/worktrees/<name>/gitdir` holds the path of the worktree's own `.git` file.
        let Ok(text) = fs::read_to_string(entry.path().join("gitdir")) else {
            continue;
        };
        if let Some(checkout) = Path::new(text.trim()).parent() {
            out.push(checkout.to_path_buf());
        }
    }
    out
}

/// The git common dir of the project that owns `target`, and whether the project is a worktree
/// that its repository no longer knows. Reads git's files directly: a worktree whose record is
/// gone is exactly the case where `git` itself refuses to answer.
fn git_link(target: &Path) -> (Option<PathBuf>, bool) {
    for dir in target.ancestors().skip(1) {
        let dot_git = dir.join(".git");
        let Ok(meta) = fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return (Some(dot_git), false);
        }
        // A worktree or submodule: `.git` is a file holding `gitdir: <path>`.
        let Some(gitdir) = fs::read_to_string(&dot_git).ok().and_then(|text| {
            let path = text
                .lines()
                .find_map(|line| line.strip_prefix(GITDIR_KEY))?;
            Some(dir.join(path.trim()))
        }) else {
            return (None, false);
        };
        if !gitdir.exists() {
            // `<repo>/.git/worktrees/<name>` is gone; the path still names the repository.
            let family = gitdir
                .parent()
                .filter(|worktrees| worktrees.file_name().is_some_and(|n| n == WORKTREES_DIR))
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            return (family, true);
        }
        let common = match fs::read_to_string(gitdir.join("commondir")) {
            Ok(relative) => gitdir.join(relative.trim()),
            Err(_) => gitdir,
        };
        return (Some(common.canonicalize().unwrap_or(common)), false);
    }
    (None, false)
}
