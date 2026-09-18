//! `~/.config/cargo-tare/config.toml`: what `run` does when no flags say otherwise. Every key is
//! optional, unknown keys are an error (a typo that silently does nothing is worse than a stop),
//! and a flag always wins over the file.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Under `$XDG_CONFIG_HOME`, or `$HOME/.config` when that is not set.
const RELATIVE: &str = "cargo-tare/config.toml";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// What `run` searches when the command line names no root.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    /// Lossy passes to enable, as `--lossy` would. They still need their thresholds.
    #[serde(default)]
    pub lossy: Vec<String>,
    /// Leave files younger than this alone, in seconds; both lossless passes.
    pub min_age: Option<u64>,
    /// Leave files smaller than this alone; both lossless passes.
    pub min_size: Option<u64>,
    /// Compare targets of different repositories too, as `--across-families` does.
    #[serde(default)]
    pub across_families: bool,
    #[serde(default)]
    pub evict: Evict,
    #[serde(default)]
    pub incremental: Incremental,
    /// Per-family overrides, keyed by the family's dir: the git common dir of the repository and
    /// its worktrees, or the target dir itself when there is no repository.
    #[serde(default)]
    pub family: BTreeMap<PathBuf, Family>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Evict {
    pub idle_days: Option<u64>,
    pub max_total_gib: Option<u64>,
    /// Take a target dir whole once every profile dir of it is evicted.
    #[serde(default)]
    pub whole_target: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Incremental {
    pub idle_days: Option<u64>,
}

/// Only what is decided per family. The `evict` cap and the idle rules are global, because the
/// passes choose over everything under the roots at once.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Family {
    /// Leave this family alone entirely.
    #[serde(default)]
    pub skip: bool,
}

impl Config {
    /// The file, or the defaults when it is not there. A file that is there and unreadable or
    /// invalid is an error: a run that silently ignores its configuration is worse than no run.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).context(format!("reading {}", path.display())),
        };
        toml::from_str(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn skips(&self, family: &Path) -> bool {
        self.family.get(family).is_some_and(|family| family.skip)
    }
}

/// `$XDG_CONFIG_HOME/cargo-tare/config.toml`, else `$HOME/.config/cargo-tare/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(xdg) if !xdg.is_empty() => PathBuf::from(xdg),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join(RELATIVE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_default_and_a_broken_one_is_an_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        assert_eq!(Config::load(&path).unwrap(), Config::default());

        fs::write(&path, "roots = 3\n").unwrap();
        let error = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(error.contains("config.toml"), "{error}");
        assert!(error.contains("roots"), "{error}");
    }

    #[test]
    fn an_unknown_key_names_itself() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "min-aeg = 60\n").unwrap();

        let error = format!("{:#}", Config::load(&path).unwrap_err());

        assert!(error.contains("min-aeg"), "{error}");
    }

    #[test]
    fn the_keys_are_kebab_case_and_families_are_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(
            &path,
            "roots = [\"/a\", \"/b\"]\n\
             lossy = [\"evict\"]\n\
             min-age = 60\n\
             min-size = 4096\n\
             [evict]\n\
             idle-days = 30\n\
             max-total-gib = 50\n\
             [incremental]\n\
             idle-days = 7\n\
             [family.\"/a/repo\"]\n\
             skip = true\n",
        )
        .unwrap();

        let config = Config::load(&path).unwrap();

        assert_eq!(config.roots, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(config.lossy, ["evict"]);
        assert_eq!((config.min_age, config.min_size), (Some(60), Some(4096)));
        assert_eq!(config.evict.idle_days, Some(30));
        assert_eq!(config.evict.max_total_gib, Some(50));
        assert_eq!(config.incremental.idle_days, Some(7));
        assert!(config.skips(Path::new("/a/repo")));
        assert!(!config.skips(Path::new("/a/other")));
    }
}
