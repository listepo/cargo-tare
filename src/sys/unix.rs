//! Unix that is not macOS, which in practice is Linux.
//!
//! Everything the inode model needs is plain POSIX and true here. What is not decided yet is
//! sharing and compression: both exist on btrfs (`FICLONE`, `chattr +c`) and reflink exists on
//! XFS, while ext4 has neither, and which one is under a target dir is a runtime question. The
//! capabilities are therefore false until `T20` answers it per root — a dedupe that falls back
//! to a byte copy would write a second copy of every artifact and free nothing.

use std::fs::{self, Metadata, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const CAN_CLONE: bool = false;
pub const CAN_COMPRESS: bool = false;
/// No flag is read here yet, so no bit means compressed. `T20` gives this `FS_COMPR_FL`.
pub const COMPRESSED: u32 = 0;

/// `st_blocks` counts 512-byte units whatever the filesystem block size is.
const ST_BLOCK_BYTES: u64 = 512;
const PERMISSION_BITS: u32 = 0o7777;

pub fn file_id(_path: &Path, meta: &Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

pub fn nlink(meta: &Metadata) -> u64 {
    meta.nlink()
}

pub fn allocated(meta: &Metadata) -> u64 {
    meta.blocks() * ST_BLOCK_BYTES
}

/// Linux keeps its per-file flags behind `FS_IOC_GETFLAGS`, an ioctl rather than a stat field.
/// Nothing reads them yet, and a file with no flag is a file the engine may replace.
pub fn flags(_meta: &Metadata) -> u32 {
    0
}

pub fn mode(meta: &Metadata) -> u32 {
    meta.mode() & PERMISSION_BITS
}

pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))
}

pub fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

/// `fs::copy` goes through `copy_file_range`, which the kernel turns into a reflink on btrfs and
/// XFS and into a real copy on ext4. Seeding wants the copy either way; the passes do not, which
/// is why [`CAN_CLONE`] is false rather than this function being a clone.
pub fn clone_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::copy(source, destination).map(|_| ())
}

/// No transparent compression is wired up here, so there is nothing to drive and nothing to
/// report. The compress pass plans no work at all ([`CAN_COMPRESS`]); this exists so that the
/// engine above it does not need to know that.
#[derive(Default)]
pub struct Compressor;

impl Compressor {
    pub fn new() -> Self {
        Self
    }

    pub fn compress(&self, _copies: &[PathBuf]) {}

    pub fn notes(&self) -> Vec<String> {
        Vec::new()
    }
}
