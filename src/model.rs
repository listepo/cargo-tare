//! Inode model of a cargo profile dir: every regular file, grouped by inode.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

/// Cargo's per-profile lock file; its presence marks a profile dir.
pub const CARGO_LOCK_FILE: &str = ".cargo-lock";
/// Prefix of our temp files. Leftovers of a crashed run are removed by the next one.
pub const TMP_PREFIX: &str = ".tare-tmp-";

const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
/// Gradle, uv and others write the same tag file; only cargo writes this sentence.
const CARGO_TAG_MARK: &str = "created by cargo";
/// `<target>/<triple>/<profile>/.cargo-lock` is the deepest place a profile lock lives.
const PROFILE_LOCK_MAX_DEPTH: usize = 3;
/// `st_blocks` counts 512-byte units whatever the filesystem block size is.
pub const ST_BLOCK_BYTES: u64 = 512;
const PERMISSION_BITS: u32 = 0o7777;

/// Identity and version of a file. Any rewrite by cargo or rustc changes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: SystemTime,
}

impl Stamp {
    pub fn of(meta: &Metadata) -> io::Result<Self> {
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime: meta.modified()?,
        })
    }

    /// Never follows a symlink: a path swapped for a link reads as changed.
    pub fn read(path: &Path) -> io::Result<Self> {
        Self::of(&fs::symlink_metadata(path)?)
    }
}

/// One inode with every path to it found inside the profile dir.
#[derive(Clone, Debug)]
pub struct Inode {
    pub stamp: Stamp,
    pub mode: u32,
    /// BSD file flags (`st_flags`); 0 off macOS.
    pub flags: u32,
    /// Link count from the filesystem. More than `paths.len()` means links we did not find.
    pub nlink: u64,
    /// Allocated bytes, not logical length.
    pub allocated: u64,
    pub paths: Vec<PathBuf>,
}

impl Inode {
    fn of(meta: &Metadata) -> io::Result<Self> {
        Ok(Self {
            stamp: Stamp::of(meta)?,
            mode: meta.mode() & PERMISSION_BITS,
            flags: bsd_flags(meta),
            nlink: meta.nlink(),
            allocated: meta.blocks() * ST_BLOCK_BYTES,
            paths: Vec::new(),
        })
    }

    /// The inode at `path` as it is now, knowing only this one path. Never follows a symlink.
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut inode = Self::of(&fs::symlink_metadata(path)?)?;
        inode.paths.push(path.to_path_buf());
        Ok(inode)
    }
}

#[derive(Debug)]
pub struct Profile {
    pub dir: PathBuf,
    pub inodes: Vec<Inode>,
    pub stale_temps: Vec<PathBuf>,
}

#[cfg(target_os = "macos")]
fn bsd_flags(meta: &Metadata) -> u32 {
    std::os::macos::fs::MetadataExt::st_flags(meta)
}

#[cfg(not(target_os = "macos"))]
fn bsd_flags(_: &Metadata) -> u32 {
    0
}

/// Walks one profile dir. Symlinks are not followed and other devices are not entered.
pub fn scan(dir: &Path) -> io::Result<Profile> {
    let mut by_inode: HashMap<(u64, u64), Inode> = HashMap::new();
    let mut stale_temps = Vec::new();
    for entry in WalkDir::new(dir).follow_links(false).same_file_system(true) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.file_name() == CARGO_LOCK_FILE {
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            stale_temps.push(entry.into_path());
            continue;
        }
        let inode = Inode::of(&entry.metadata()?)?;
        by_inode
            .entry((inode.stamp.dev, inode.stamp.ino))
            .or_insert(inode)
            .paths
            .push(entry.into_path());
    }
    let mut inodes: Vec<Inode> = by_inode.into_values().collect();
    for inode in &mut inodes {
        inode.paths.sort();
    }
    inodes.sort_by(|a, b| a.paths.cmp(&b.paths));
    Ok(Profile {
        dir: dir.to_path_buf(),
        inodes,
        stale_temps,
    })
}

/// True when cargo itself tagged `dir` as a target or build dir.
pub fn is_cargo_target(dir: &Path) -> bool {
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
        if entry.file_name() == CARGO_LOCK_FILE
            && let Some(parent) = entry.path().parent()
        {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs.sort();
    Ok(dirs)
}
