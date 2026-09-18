//! The cargo home (`~/.cargo`), which no competitor compresses: `registry/src` holds every
//! dependency's unpacked sources and `git/checkouts` the same for git dependencies — plain text,
//! the most compressible bytes on the machine, and a cache of immutable sources cargo can fetch
//! again. Compression here is as lossless as it gets.
//!
//! What guards it is cargo's own `.package-cache`, which cargo holds while it fetches or
//! extracts. `registry/cache` is left alone: `.crate` files are already compressed archives.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::UF_COMPRESSED;
use crate::model;

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
    for dir in dirs(home) {
        for inode in model::scan(&dir)?.inodes {
            stats.allocated_bytes += inode.allocated;
            stats.logical_bytes += inode.stamp.size;
            if inode.flags & UF_COMPRESSED != 0 {
                stats.compressed_bytes += inode.allocated;
            } else if inode.stamp.size >= crate::compress::DEFAULT_MIN_SIZE {
                stats.compressible_bytes += inode.allocated;
            }
        }
    }
    Ok(stats)
}
