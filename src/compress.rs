//! The compress pass: transparent APFS compression of files that the next build will not
//! rewrite. The engine hands the backend private copies only (`Action::Compress`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use applesauce::FileCompressor;
use applesauce::compressor::Kind;
use applesauce::progress::{Progress, SkipReason, Task};

use crate::engine::{Action, Pass};
use crate::index::HashIndex;
use crate::model::{Inode, Profile};

/// Two APFS blocks: below that there is at most one block to win.
pub const NAME: &str = "compress";
pub const DEFAULT_MIN_SIZE: u64 = 8 * 1024;
/// Younger files are likely to be rewritten by the next build, uncompressed.
pub const DEFAULT_MIN_AGE: Duration = Duration::from_secs(60 * 60);
/// LZFSE: the best ratio of the three codecs at a read speed T2 could not tell from plain.
const KIND: Kind = Kind::Lzfse;
/// The backend's own defaults.
const LEVEL: u32 = 5;
/// A copy that does not get below this share of its size stays uncompressed.
const MIN_RATIO: f64 = 0.95;
/// The backend reads the compressed copy back and compares it before it lets it stand.
const VERIFY: bool = true;

pub struct Compress<'a> {
    pub min_size: u64,
    pub min_age: Duration,
    index: &'a RefCell<HashIndex>,
    backend: RefCell<Option<FileCompressor>>,
    notes: Notes,
}

impl<'a> Compress<'a> {
    pub fn new(index: &'a RefCell<HashIndex>) -> Self {
        Self {
            min_size: DEFAULT_MIN_SIZE,
            min_age: DEFAULT_MIN_AGE,
            index,
            backend: RefCell::new(None),
            notes: Notes::default(),
        }
    }

    /// What the backend said about copies it did not compress, one line each. The paths are
    /// those of the engine's temp copies: the dir tells where, the engine's report tells which.
    pub fn notes(&self) -> Vec<String> {
        self.notes.0.lock().map(|n| n.clone()).unwrap_or_default()
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
        let mut backend = self.backend.borrow_mut();
        // ponytail: a copy the backend refuses is tried again on every run; remember refusals
        // in the index if the benchmarks show them.
        backend
            .get_or_insert_with(FileCompressor::new)
            .recursive_compress(
                copies.iter().map(PathBuf::as_path),
                KIND,
                MIN_RATIO,
                LEVEL,
                &self.notes,
                VERIFY,
            );
    }
}

#[derive(Clone, Default)]
struct Notes(Arc<Mutex<Vec<String>>>);

impl Notes {
    fn push(&self, path: &Path, message: &str) {
        if let Ok(mut notes) = self.0.lock() {
            notes.push(format!("{}: {message}", path.display()));
        }
    }
}

impl Progress for Notes {
    type Task = FileNotes;

    fn error(&self, path: &Path, message: &str) {
        self.push(path, message);
    }

    fn file_skipped(&self, path: &Path, why: SkipReason) {
        self.push(path, &why.to_string());
    }

    fn file_task(&self, path: &Path, _size: u64) -> FileNotes {
        FileNotes {
            notes: self.clone(),
            path: path.to_path_buf(),
        }
    }
}

struct FileNotes {
    notes: Notes,
    path: PathBuf,
}

impl Task for FileNotes {
    fn increment(&self, _amt: u64) {}

    fn error(&self, message: &str) {
        self.notes.push(&self.path, message);
    }

    fn not_compressible_enough(&self, path: &Path) {
        self.notes.push(path, "not compressible enough");
    }
}
