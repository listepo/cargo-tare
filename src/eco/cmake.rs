//! CMake: a build dir holding `CMakeCache.txt`, whatever the generator. No generator takes a
//! lock a second process would honour, so the whole dir is one [`Guard::Quiet`] unit. The cache
//! records the source dir, which makes the owner one line of text away.

use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::{Ecosystem, Guard, Owner, Policy, Sharing};

pub const CACHE: &str = "CMakeCache.txt";
/// The cache entry naming the source dir the build dir was configured from.
const HOME_KEY: &str = "CMAKE_HOME_DIRECTORY:INTERNAL=";

/// CMake build dirs.
pub struct Cmake;

pub static CMAKE: Cmake = Cmake;

impl Ecosystem for Cmake {
    fn name(&self) -> &'static str {
        "cmake"
    }

    /// Not an in-source build (`cmake .`): there the build dir is the source tree, and a lossy
    /// pass removing the unit would remove the sources with it.
    /// Compared as real paths: the cache holds one, the walk may have come through a symlink.
    fn claim(&self, dir: &Path) -> bool {
        let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.owner(dir)
            .is_some_and(|owner| !real(&owner.project).starts_with(real(dir)))
    }

    /// The source dir the cache names. A build dir sits anywhere — inside the source tree, next
    /// to it, in `/tmp` — so where it is says nothing.
    fn owner(&self, build_dir: &Path) -> Option<Owner> {
        let cache = fs::File::open(build_dir.join(CACHE)).ok()?;
        let project = BufReader::new(cache)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| Some(PathBuf::from(line.strip_prefix(HOME_KEY)?.trim())))?;
        Some(Owner { project })
    }

    fn manifest(&self, project: &Path) -> Option<PathBuf> {
        Some(project.join("CMakeLists.txt"))
    }

    fn units(&self, build_dir: &Path) -> io::Result<Vec<PathBuf>> {
        if !self.claim(build_dir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no {CACHE}, or an in-source build: not a CMake build dir"),
            ));
        }
        Ok(vec![build_dir.to_path_buf()])
    }

    fn guard(&self, _unit: &Path) -> Guard {
        Guard::Quiet
    }

    fn last_used(&self, unit: &Path) -> Option<u64> {
        super::cargo::last_built(unit)
    }

    /// The build drivers: each runs for the whole build, in the build dir or around it.
    fn tools(&self) -> &'static [&'static str] {
        &["cmake", "ninja", "make", "gmake", "ctest"]
    }

    /// Objects and archives may be rewritten in place (`ar` updates an archive), so equal files
    /// are shared by clones only.
    fn policy(&self) -> Policy {
        Policy {
            share: Sharing::ClonesOnly,
        }
    }
}
