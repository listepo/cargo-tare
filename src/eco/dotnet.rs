//! .NET: a project's `obj/` and `bin/`, next to the project file. MSBuild takes no lock, so both
//! are [`Guard::Quiet`]. Its `Copy` task overwrites a destination in place, which is how its own
//! hardlink option corrupts the NuGet cache (dotnet/msbuild#8273): equal files here are shared
//! by clones only, whatever the user asks for.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Ecosystem, Guard, Owner, Policy, Sharing};

/// Intermediate outputs; holds what restore writes.
pub const OBJ: &str = "obj";
/// Final outputs.
pub const BIN: &str = "bin";
/// Written by every restore into `obj/`; what marks a project's build dirs.
const ASSETS: &str = "project.assets.json";
/// `obj/<project file name>.nuget.dgspec.json`, also written by restore.
const DGSPEC_SUFFIX: &str = ".nuget.dgspec.json";

/// .NET SDK projects' `bin/` and `obj/`.
pub struct Dotnet;

pub static DOTNET: Dotnet = Dotnet;

impl Ecosystem for Dotnet {
    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn claim(&self, dir: &Path) -> bool {
        let obj = match dir.file_name().and_then(OsStr::to_str) {
            Some(OBJ) => dir.to_path_buf(),
            Some(BIN) => dir.with_file_name(OBJ),
            _ => return false,
        };
        obj.join(ASSETS).is_file()
    }

    fn owner(&self, build_dir: &Path) -> Option<Owner> {
        let project = build_dir.parent()?.to_path_buf();
        Some(Owner { project })
    }

    /// The project file restore recorded. A project dir holds any number of them, and `obj/`
    /// keeps the record of a project renamed since: an existing one wins, so the project counts
    /// as gone only when every project file restore recorded is.
    fn manifest(&self, project: &Path) -> Option<PathBuf> {
        let mut recorded: Vec<PathBuf> = fs::read_dir(project.join(OBJ))
            .ok()?
            .filter_map(|entry| {
                let name = entry.ok()?.file_name();
                let file = name.to_str()?.strip_suffix(DGSPEC_SUFFIX)?;
                Some(project.join(file))
            })
            .collect();
        recorded.sort();
        let existing = recorded
            .iter()
            .position(|file| fs::symlink_metadata(file).is_ok());
        recorded.into_iter().nth(existing.unwrap_or(0))
    }

    /// The whole dir: MSBuild writes all of it in one build, and nothing guards a part of it.
    fn units(&self, build_dir: &Path) -> io::Result<Vec<PathBuf>> {
        if !self.claim(build_dir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no {OBJ}/{ASSETS}: not a .NET project's build dir"),
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

    /// `dotnet build` and `dotnet msbuild` are `dotnet`, and so are the worker nodes and the
    /// compiler server they leave running; `MSBuild` and `VBCSCompiler` where they run on their
    /// own.
    fn tools(&self) -> &'static [&'static str] {
        &["dotnet", "MSBuild", "VBCSCompiler"]
    }

    /// Never a hardlink: `Copy` would write through it into every other path.
    fn policy(&self) -> Policy {
        Policy {
            share: Sharing::ClonesOnly,
        }
    }
}
