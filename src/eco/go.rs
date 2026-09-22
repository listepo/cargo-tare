//! Go's module cache (`go env GOMODCACHE`): every module the `go` command downloaded, unpacked
//! once into `<path>@<version>` and then only read — the Go counterpart of the cargo home's
//! `registry/src`. `GOCACHE`, the build cache, is a content-addressed store (`store.rs`).
//!
//! A module dir never gets other bytes: `go` unpacks into a temp dir and renames it into place,
//! and verifies it later by the hash of its files alone. What sets it apart is that `go` makes
//! every dir read-only so nobody edits a dependency by accident; `compress` lifts a dir's write
//! bit for one batch and puts it back (`Ecosystem::lifts_read_only_dirs`).

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Ecosystem, Guard, Policy, Sharing};

/// Where `go` keeps what it downloaded, zipped, and its VCS clones: left alone.
pub const CACHE_DIR: &str = "cache";
/// What every module cache `go` has used holds.
pub const DOWNLOADS: &str = "cache/download";

/// The adapter of a named module cache.
pub struct ModCache;

pub static MOD_CACHE: ModCache = ModCache;

impl Ecosystem for ModCache {
    fn name(&self) -> &'static str {
        "go modcache"
    }

    /// The unpacked modules: every top-level dir but [`CACHE_DIR`].
    fn units(&self, build_dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut units = Vec::new();
        for entry in fs::read_dir(build_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.file_name() != OsStr::new(CACHE_DIR) {
                units.push(entry.path());
            }
        }
        Ok(units)
    }

    fn guard(&self, _unit: &Path) -> Guard {
        Guard::Immutable
    }

    fn lifts_read_only_dirs(&self) -> bool {
        true
    }

    fn policy(&self) -> Policy {
        Policy {
            share: Sharing::ClonesOnly,
        }
    }
}

/// Why `dir` is not a module cache this tool may compress, or `None` when it may.
pub fn check(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return Some("not a directory".into());
    }
    if !dir.join(DOWNLOADS).is_dir() {
        return Some(format!("no {DOWNLOADS} in it: not a Go module cache"));
    }
    None
}
