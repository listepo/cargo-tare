//! Cargo: target and build dirs tagged by cargo, guarded per profile dir by `.cargo-lock`, and
//! the cargo home guarded as a whole by `.package-cache`. Also the passes and the reports only
//! cargo has.

pub mod advise;
pub mod doc;
pub mod home;
pub mod incremental;
pub mod toolchains;

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use super::{Ecosystem, Guard, Owner, Policy, Sharing};

/// Cargo's per-profile lock file; its presence marks a profile dir.
pub const LOCK_FILE: &str = ".cargo-lock";
/// The target dir of a workspace, as cargo names it by default.
pub const TARGET: &str = "target";

const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
/// Gradle, uv and others write the same tag file; only cargo writes this sentence.
const CARGO_TAG_MARK: &str = "created by cargo";
/// `<target>/<triple>/<profile>/.cargo-lock` is the deepest place a profile lock lives.
const PROFILE_LOCK_MAX_DEPTH: usize = 3;

/// Cargo target dirs and `build-dir`s.
pub struct Cargo;

pub static CARGO: Cargo = Cargo;

impl Ecosystem for Cargo {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn claim(&self, dir: &Path) -> bool {
        is_target(dir)
    }

    /// The dir above the target: the workspace root when the target sits where cargo puts it
    /// by default. A target moved elsewhere names the dir it was moved into, as before.
    fn owner(&self, build_dir: &Path) -> Option<Owner> {
        let project = build_dir.parent()?.to_path_buf();
        Some(Owner { project })
    }

    fn build_dir(&self, project: &Path) -> Option<PathBuf> {
        Some(project.join(TARGET))
    }

    fn units(&self, build_dir: &Path) -> io::Result<Vec<PathBuf>> {
        profile_dirs(build_dir)
    }

    fn guard(&self, unit: &Path) -> Guard {
        Guard::Lock(unit.join(LOCK_FILE))
    }

    fn private(&self, name: &OsStr) -> bool {
        name == LOCK_FILE
    }

    /// `incremental/` is a cache of one checkout's build that cargo will not use in another.
    fn volatile(&self, name: &OsStr) -> bool {
        name == incremental::DIR
    }

    fn last_used(&self, unit: &Path) -> Option<u64> {
        last_built(unit)
    }

    /// Artifacts may be rewritten in place by a later build, so a hardlink between two of them
    /// is the user's call.
    fn policy(&self) -> Policy {
        Policy {
            share: Sharing::LinkOptIn,
        }
    }
}

/// True when cargo itself tagged `dir` as a target or build dir.
pub fn is_target(dir: &Path) -> bool {
    fs::read_to_string(dir.join(CACHEDIR_TAG)).is_ok_and(|tag| tag.contains(CARGO_TAG_MARK))
}

/// Profile dirs of a cargo target dir. Refuses a dir that cargo did not tag as its own.
pub fn profile_dirs(target: &Path) -> io::Result<Vec<PathBuf>> {
    let tag = fs::read_to_string(target.join(CACHEDIR_TAG))?;
    if !tag.contains(CARGO_TAG_MARK) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{CACHEDIR_TAG} was not written by cargo"),
        ));
    }
    let mut dirs = Vec::new();
    let walk = WalkDir::new(target)
        .follow_links(false)
        .same_file_system(true)
        .max_depth(PROFILE_LOCK_MAX_DEPTH);
    for entry in walk {
        let entry = entry?;
        if entry.file_name() == LOCK_FILE
            && let Some(parent) = entry.path().parent()
        {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs.sort();
    Ok(dirs)
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
