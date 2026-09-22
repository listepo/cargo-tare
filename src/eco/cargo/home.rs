//! The cargo home (`~/.cargo`), which no competitor compresses: `registry/src` holds every
//! dependency's unpacked sources and `git/checkouts` the same for git dependencies — plain text,
//! the most compressible bytes on the machine, and a cache of immutable sources cargo can fetch
//! again. Compression here is as lossless as it gets.
//!
//! What guards it is cargo's own `.package-cache`, which cargo holds while it fetches or
//! extracts. `registry/cache` is left alone: `.crate` files are already compressed archives.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::eco::{Ecosystem, Guard, Policy, Sharing};
use crate::model;
use crate::sys::COMPRESSED;

/// Cargo's lock for its whole home, directly inside it.
pub const LOCK_FILE: &str = ".package-cache";
/// The dirs worth compressing, in the order the report lists them. `registry/cache` is not one
/// of them, and neither is anything else cargo stores already packed.
pub const DIRS: [&str; 2] = ["registry/src", "git/checkouts"];

/// The cargo home to work on: `--cargo-home <DIR>`, else `$CARGO_HOME`, else `~/.cargo`. The
/// path is not checked here; `dirs` reports what of it exists.
pub fn path(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
}

/// Those of [`DIRS`] that exist in `home`.
pub fn dirs(home: &Path) -> Vec<PathBuf> {
    DIRS.iter()
        .map(|relative| home.join(relative))
        .filter(|dir| dir.is_dir())
        .collect()
}

/// The cargo home as a unit of work: its [`DIRS`] under one [`LOCK_FILE`]. Never found by a
/// walk; a run names it.
pub struct Home {
    pub home: PathBuf,
}

impl Ecosystem for Home {
    fn name(&self) -> &'static str {
        "cargo home"
    }

    fn units(&self, _build_dir: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(dirs(&self.home))
    }

    fn guard(&self, _unit: &Path) -> Guard {
        Guard::Shared(self.home.join(LOCK_FILE))
    }

    fn private(&self, name: &OsStr) -> bool {
        super::CARGO.private(name)
    }

    fn policy(&self) -> Policy {
        Self::POLICY
    }
}

impl Home {
    /// Cargo replaces a source dir instead of rewriting its files, so a hardlink is safe.
    pub const POLICY: Policy = Policy {
        share: Sharing::LinkSafe,
    };
}

/// What `status` reports about the cargo home, measured the same way targets are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub home: PathBuf,
    /// What `du` reports for the dirs the pass would work on.
    pub allocated_bytes: u64,
    pub logical_bytes: u64,
    pub compressed_bytes: u64,
    /// Allocated bytes of files the compress pass would still look at.
    pub compressible_bytes: u64,
}

/// Reads the dirs of `home`; takes no lock and changes nothing, as the rest of the inventory.
pub fn inspect(home: &Path) -> io::Result<Stats> {
    let mut stats = Stats {
        home: home.to_path_buf(),
        ..Stats::default()
    };
    let adapter = Home {
        home: home.to_path_buf(),
    };
    for dir in dirs(home) {
        for inode in model::scan(&dir, &adapter)?.inodes {
            stats.allocated_bytes += inode.allocated;
            stats.logical_bytes += inode.stamp.size;
            if inode.flags & COMPRESSED != 0 {
                stats.compressed_bytes += inode.allocated;
            } else if inode.stamp.size >= crate::compress::DEFAULT_MIN_SIZE
                && crate::sys::caps(&dir).compress
            {
                stats.compressible_bytes += inode.allocated;
            }
        }
    }
    Ok(stats)
}
