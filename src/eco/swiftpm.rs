//! SwiftPM: a package's `.build` scratch dir, guarded as a whole by the lock `swift build` takes
//! on it. The lock is not in the dir: TSCBasic's `FileLock` puts it in the temp dir, named after
//! the scratch dir's path.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Ecosystem, Guard, Owner, Policy, Sharing};

/// The scratch dir of a package, as SwiftPM names it by default.
pub const SCRATCH: &str = ".build";
/// Written by every SwiftPM that resolves a package; what marks a scratch dir.
const WORKSPACE_STATE: &str = "workspace-state.json";
/// Where the default build system (swiftbuild) writes.
const OUT: &str = "out";
/// The deprecated native build system writes into `<triple>/<configuration>`.
const CONFIGURATIONS: [&str; 2] = ["debug", "release"];
/// The compilation cache's databases: mmapped, sparse, many GiB of logical size. Its own store,
/// not files a pass may rewrite.
const COMPILATION_CACHE: &str = "CompilationCache.noindex";
/// `FileLock` keeps at most this many bytes of the lock file name: its last ones.
const NAME_MAX: usize = 255;

/// SwiftPM scratch dirs.
pub struct SwiftPm;

pub static SWIFTPM: SwiftPm = SwiftPm;

impl Ecosystem for SwiftPm {
    fn name(&self) -> &'static str {
        "swiftpm"
    }

    fn claim(&self, dir: &Path) -> bool {
        dir.file_name() == Some(OsStr::new(SCRATCH)) && dir.join(WORKSPACE_STATE).is_file()
    }

    fn owner(&self, build_dir: &Path) -> Option<Owner> {
        let project = build_dir.parent()?.to_path_buf();
        Some(Owner { project })
    }

    fn manifest(&self, project: &Path) -> Option<PathBuf> {
        Some(project.join("Package.swift"))
    }

    /// The build outputs. Dependency checkouts and clones stay out: sources, not build products.
    /// So does `index-build`, sourcekit-lsp's scratch dir, which is locked under its own name.
    fn units(&self, build_dir: &Path) -> io::Result<Vec<PathBuf>> {
        if !self.claim(build_dir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no {WORKSPACE_STATE}: not a SwiftPM scratch dir"),
            ));
        }
        let mut units = Vec::new();
        for entry in fs::read_dir(build_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let triple = CONFIGURATIONS.iter().any(|c| dir.join(c).is_dir());
            if entry.file_name() == OUT || triple {
                units.push(dir);
            }
        }
        units.sort();
        Ok(units)
    }

    /// Every unit of a scratch dir under the one lock `swift build` holds for the whole build.
    fn guard(&self, unit: &Path) -> Guard {
        let scratch = unit.parent().unwrap_or(unit);
        let scratch = scratch
            .canonicalize()
            .unwrap_or_else(|_| scratch.to_path_buf());
        Guard::Shared(crate::sys::temp_dir().join(lock_name(&scratch)))
    }

    fn private(&self, name: &OsStr) -> bool {
        name == COMPILATION_CACHE
    }

    fn last_used(&self, unit: &Path) -> Option<u64> {
        super::cargo::last_built(unit)
    }

    /// Nothing says a build never rewrites an output in place.
    fn policy(&self) -> Policy {
        Policy {
            share: Sharing::ClonesOnly,
        }
    }
}

/// The name of the lock file of `scratch`: its path with every `/` as `_`, plus `.lock`, cut to
/// its last [`NAME_MAX`] bytes without splitting a character.
pub fn lock_name(scratch: &Path) -> String {
    let name = format!("{}.lock", scratch.to_string_lossy().replace('/', "_"));
    let mut start = name.len().saturating_sub(NAME_MAX);
    while !name.is_char_boundary(start) {
        start += 1;
    }
    name[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_is_named_after_the_path() {
        assert_eq!(lock_name(Path::new("/w/app/.build")), "_w_app_.build.lock");
    }

    #[test]
    fn a_long_name_keeps_its_end_and_whole_characters() {
        // 2 + 249 + 5 = 256 bytes: the cut falls inside the two bytes of "é", which goes whole.
        let name = lock_name(Path::new(&format!("é{}", "a".repeat(249))));
        assert_eq!(name, format!("{}.lock", "a".repeat(249)));
        let name = lock_name(Path::new(&format!("/{}/app/.build", "b".repeat(300))));
        assert_eq!(name.len(), NAME_MAX);
        assert!(name.ends_with("b_app_.build.lock"));
    }
}
