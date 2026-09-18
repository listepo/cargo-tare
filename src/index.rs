//! Persistent content-hash cache, so a run after a build hashes only the new inodes.
//!
//! A cache and nothing more: a missing, truncated or foreign file reads as empty, and losing it
//! costs one full rehash plus one redundant round of cloning.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::model::Stamp;

pub const HASH_BYTES: usize = 32;
pub type Hash = [u8; HASH_BYTES];

/// File format: magic, then fixed-size little-endian records. Bump the digit on any change.
const MAGIC: &[u8] = b"TAREIDX1";
const U64_BYTES: usize = 8;
const U32_BYTES: usize = 4;
/// dev, ino, size, mtime seconds (u64 each), mtime nanoseconds (u32), hash, shared (u8).
const RECORD_BYTES: usize = 4 * U64_BYTES + U32_BYTES + HASH_BYTES + 1;
const TEMP_EXTENSION: &str = "tmp";
const NANOS_PER_SEC: u32 = 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub size: u64,
    pub mtime: SystemTime,
    pub hash: Hash,
    /// This tool cloned the inode, or cloned from it: its blocks are already shared.
    /// The filesystem cannot be asked, so without the mark every run would clone it again.
    pub shared: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct HashIndex {
    entries: HashMap<(u64, u64), Entry>,
}

impl HashIndex {
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| Self::decode(&bytes))
            .unwrap_or_default()
    }

    /// The entry for this exact version of the inode; a rewritten file misses.
    pub fn get(&self, stamp: &Stamp) -> Option<&Entry> {
        self.entries
            .get(&(stamp.dev, stamp.ino))
            .filter(|e| e.size == stamp.size && e.mtime == stamp.mtime)
    }

    pub fn put(&mut self, stamp: &Stamp, hash: Hash, shared: bool) {
        let entry = Entry {
            size: stamp.size,
            mtime: stamp.mtime,
            hash,
            shared,
        };
        self.entries.insert((stamp.dev, stamp.ino), entry);
    }

    pub fn mark_shared(&mut self, stamp: &Stamp) {
        if let Some(entry) = self.entries.get_mut(&(stamp.dev, stamp.ino))
            && entry.size == stamp.size
            && entry.mtime == stamp.mtime
        {
            entry.shared = true;
        }
    }

    /// Forget an inode that no longer exists.
    pub fn remove(&mut self, stamp: &Stamp) {
        self.entries.remove(&(stamp.dev, stamp.ino));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Temp file plus `rename`: a crash never leaves a half-written index.
    // ponytail: entries of deleted targets are never expired; add a last-seen field and drop
    // old ones if the file grows past a few MB.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(TEMP_EXTENSION);
        fs::write(&temp, self.encode())?;
        fs::rename(&temp, path)
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAGIC.len() + self.entries.len() * RECORD_BYTES);
        out.extend_from_slice(MAGIC);
        for (&(dev, ino), entry) in &self.entries {
            // A pre-1970 mtime is not worth a format with signed seconds; just do not cache it.
            let Ok(mtime) = entry.mtime.duration_since(SystemTime::UNIX_EPOCH) else {
                continue;
            };
            for value in [dev, ino, entry.size, mtime.as_secs()] {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.extend_from_slice(&mtime.subsec_nanos().to_le_bytes());
            out.extend_from_slice(&entry.hash);
            out.push(u8::from(entry.shared));
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let records = bytes.strip_prefix(MAGIC)?;
        let (records, rest) = records.as_chunks::<RECORD_BYTES>();
        if !rest.is_empty() {
            return None;
        }
        let mut entries = HashMap::new();
        for record in records {
            let mut record: &[u8] = record;
            let mut take = |n: usize| {
                let (head, tail) = record.split_at(n);
                record = tail;
                head
            };
            let mut u64_field = || u64::from_le_bytes(take(U64_BYTES).try_into().unwrap());
            let (dev, ino, size, secs) = (u64_field(), u64_field(), u64_field(), u64_field());
            let nanos = u32::from_le_bytes(take(U32_BYTES).try_into().unwrap());
            let hash: Hash = take(HASH_BYTES).try_into().unwrap();
            let shared = take(1)[0] != 0;
            if nanos >= NANOS_PER_SEC {
                return None; // `Duration::new` would carry, and panic on overflow
            }
            let mtime = SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))?;
            let entry = Entry {
                size,
                mtime,
                hash,
                shared,
            };
            entries.insert((dev, ino), entry);
        }
        Some(Self { entries })
    }
}
