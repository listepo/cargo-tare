//! The lossy `incremental` pass: the `incremental/` cache of profile dirs nobody has built in
//! for a while. Cargo keeps it only for workspace members, so what it costs to drop is one
//! non-incremental rebuild of the crates you wrote — and only in a profile you are not using.

use std::path::PathBuf;

use crate::engine::{Action, Pass};
use crate::inventory::ProfileInfo;
use crate::model::Profile;

pub const NAME: &str = "incremental";
/// Cargo's own name for the dir, inside every profile dir it compiles workspace members in.
pub const DIR: &str = "incremental";
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// A profile dir whose incremental cache is old enough to drop, with its age in days.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Idle {
    pub profile: ProfileInfo,
    pub days: u64,
}

impl Idle {
    fn dir(&self) -> PathBuf {
        self.profile.dir.join(DIR)
    }
}

/// Which incremental caches to drop. Pure except for the `exists` check on the dir itself:
/// a profile with no cache has nothing to plan. A profile whose last build is unknown is never
/// chosen, as in `evict`.
pub fn select(profiles: &[ProfileInfo], now_unix: u64, idle_days: u64) -> Vec<Idle> {
    profiles
        .iter()
        .filter(|profile| profile.dir.join(DIR).is_dir())
        .filter_map(|profile| {
            let days = now_unix.saturating_sub(profile.last_built_unix?) / SECS_PER_DAY;
            (days >= idle_days).then(|| Idle {
                profile: profile.clone(),
                days,
            })
        })
        .collect()
}

pub struct Incremental {
    chosen: Vec<Idle>,
}

impl Incremental {
    /// `chosen` comes from [`select`] over an inventory taken before any lock was held.
    pub fn new(chosen: Vec<Idle>) -> Self {
        Self { chosen }
    }
}

impl Pass for Incremental {
    fn name(&self) -> &'static str {
        NAME
    }

    fn lossy(&self) -> bool {
        true
    }

    fn plan(&self, profiles: &[Profile]) -> Vec<Action> {
        // Now that the lock is ours: a build that ran after the inventory keeps its cache.
        let unbuilt = |info: &ProfileInfo| {
            profiles
                .iter()
                .find(|profile| profile.dir == info.dir)
                .is_some_and(|profile| profile.last_used == info.last_built_unix)
        };
        self.chosen
            .iter()
            .filter(|idle| unbuilt(&idle.profile) && idle.dir().is_dir())
            .map(|idle| Action::Remove {
                dir: idle.dir(),
                reason: format!(
                    "incremental cache of a profile with no build for {} days",
                    idle.days
                ),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    const NOW: u64 = 1_000 * SECS_PER_DAY;

    fn profile(root: &Path, name: &str, built_days_ago: u64, cache: bool) -> ProfileInfo {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        if cache {
            fs::create_dir_all(dir.join(DIR)).unwrap();
        }
        ProfileInfo {
            dir,
            allocated_bytes: 0,
            last_built_unix: Some(NOW - built_days_ago * SECS_PER_DAY),
        }
    }

    fn names(chosen: &[Idle]) -> Vec<String> {
        let name = |idle: &Idle| {
            idle.profile
                .dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        chosen.iter().map(name).collect()
    }

    #[test]
    fn only_idle_profiles_that_have_a_cache_are_chosen() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let profiles = [
            profile(root, "fresh", 0, true),
            profile(root, "idle", 30, true),
            profile(root, "idle-without-cache", 30, false),
        ];

        let chosen = select(&profiles, NOW, 7);

        assert_eq!(names(&chosen), ["idle"]);
        assert_eq!(chosen[0].days, 30);
    }

    #[test]
    fn a_profile_with_an_unknown_last_build_is_never_chosen() {
        let tmp = TempDir::new().unwrap();
        let mut unknown = profile(tmp.path(), "unknown", 0, true);
        unknown.last_built_unix = None;

        assert!(select(&[unknown], NOW, 0).is_empty());
    }
}
