//! The compress pass: transparent filesystem compression of files that the next build will not
//! rewrite. The engine hands the backend private copies only (`Action::Compress`).

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::engine::{Action, Pass};
use crate::index::HashIndex;
use crate::model::{Inode, Profile};
use crate::sys::{self, Compressor};

/// Two APFS blocks: below that there is at most one block to win.
pub const NAME: &str = "compress";
pub const DEFAULT_MIN_SIZE: u64 = 8 * 1024;
/// Younger files are likely to be rewritten by the next build, uncompressed.
pub const DEFAULT_MIN_AGE: Duration = Duration::from_secs(60 * 60);

pub struct Compress<'a> {
    pub min_size: u64,
    pub min_age: Duration,
    index: &'a RefCell<HashIndex>,
    backend: Compressor,
}

impl<'a> Compress<'a> {
    pub fn new(index: &'a RefCell<HashIndex>) -> Self {
        Self {
            min_size: DEFAULT_MIN_SIZE,
            min_age: DEFAULT_MIN_AGE,
            index,
            backend: Compressor::new(),
        }
    }

    /// What the backend said about copies it did not compress, one line each. The paths are
    /// those of the engine's temp copies: the dir tells where, the engine's report tells which.
    pub fn notes(&self) -> Vec<String> {
        self.backend.notes()
    }

    fn eligible(&self, inode: &Inode, now: SystemTime) -> bool {
        inode.stamp.size >= self.min_size
            && inode.nlink == inode.paths.len() as u64
            // Already compressed, or carrying a flag that is not ours to drop.
            && inode.flags == 0
            && now
                .duration_since(inode.stamp.mtime)
                .is_ok_and(|age| age >= self.min_age)
    }
}

impl Pass for Compress<'_> {
    fn name(&self) -> &'static str {
        NAME
    }

    fn plan(&self, profiles: &[Profile]) -> Vec<Action> {
        let now = SystemTime::now();
        let index = self.index.borrow();
        profiles
            .iter()
            // No transparent compression under this dir means no copy there is worth making:
            // the engine would clone every candidate only to throw the copy away again.
            .filter(|profile| sys::caps(&profile.dir).compress)
            .flat_map(|profile| &profile.inodes)
            .filter(|inode| self.eligible(inode, now))
            // Compressing a clone un-shares it and wins nothing until its twins follow.
            // ponytail: a cluster shared while uncompressed stays uncompressed; compress the
            // canonical and re-clone the whole cluster if runs with compress off become common.
            .filter(|inode| !index.get(&inode.stamp).is_some_and(|entry| entry.shared))
            .cloned()
            .map(Action::Compress)
            .collect()
    }

    fn compress(&self, copies: &[PathBuf]) {
        self.backend.compress(copies);
    }
}
