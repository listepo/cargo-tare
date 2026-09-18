//! The lossy `evict` pass: whole profile dirs nobody has built in for a while, then the least
//! recently built ones until everything under the roots fits a size cap. Everything it deletes
//! cargo rebuilds; nothing else is ever touched.

use std::fmt;
use std::path::PathBuf;

use crate::engine::{Action, Pass};
use crate::inventory::{self, ProfileInfo};
use crate::model::Profile;

pub const NAME: &str = "evict";
const SECS_PER_DAY: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Evict a profile dir whose last build is at least this many days old.
    pub idle_days: Option<u64>,
    /// Then evict least recently built profile dirs until all of them together fit.
    pub max_total_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Idle {
        days: u64,
    },
    OverCap {
        total_bytes: u64,
        max_total_bytes: u64,
    },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle { days } => write!(f, "idle for {days} days"),
            Self::OverCap {
                total_bytes,
                max_total_bytes,
            } => write!(
                f,
                "least recently built while {total_bytes} bytes exceed the cap of {max_total_bytes}"
            ),
        }
    }
}

/// Which profile dirs to evict and why. Pure: `profiles` is everything under the roots, the cap
/// is global. A profile whose last build is unknown is never chosen.
pub fn select(
    profiles: &[ProfileInfo],
    now_unix: u64,
    limits: Limits,
) -> Vec<(ProfileInfo, Reason)> {
    let mut chosen = Vec::new();
    let mut total_bytes: u64 = profiles.iter().map(|p| p.allocated_bytes).sum();
    let mut kept: Vec<(u64, &ProfileInfo)> = Vec::new();
    for profile in profiles {
        let Some(built) = profile.last_built_unix else {
            continue;
        };
        let days = now_unix.saturating_sub(built) / SECS_PER_DAY;
        if limits.idle_days.is_some_and(|idle_days| days >= idle_days) {
            total_bytes -= profile.allocated_bytes;
            chosen.push((profile.clone(), Reason::Idle { days }));
        } else {
            kept.push((built, profile));
        }
    }
    if let Some(max_total_bytes) = limits.max_total_bytes {
        kept.sort_by(|a, b| (a.0, &a.1.dir).cmp(&(b.0, &b.1.dir)));
        for (_, profile) in kept {
            if total_bytes <= max_total_bytes {
                break;
            }
            let reason = Reason::OverCap {
                total_bytes,
                max_total_bytes,
            };
            total_bytes -= profile.allocated_bytes;
            chosen.push((profile.clone(), reason));
        }
    }
    chosen
}

pub struct Evict {
    chosen: Vec<(ProfileInfo, Reason)>,
}

impl Evict {
    /// `chosen` comes from [`select`] over an inventory taken before any lock was held.
    pub fn new(chosen: Vec<(ProfileInfo, Reason)>) -> Self {
        Self { chosen }
    }
}

impl Pass for Evict {
    fn name(&self) -> &'static str {
        NAME
    }

    fn lossy(&self) -> bool {
        true
    }

    fn plan(&self, profiles: &[Profile]) -> Vec<Action> {
        let locked = |dir: &PathBuf| profiles.iter().any(|profile| profile.dir == *dir);
        self.chosen
            .iter()
            .filter(|(info, _)| locked(&info.dir))
            // Now that the lock is ours: a build that ran after the inventory keeps its profile.
            .filter(|(info, _)| inventory::last_built(&info.dir) == info.last_built_unix)
            .map(|(info, reason)| Action::Remove {
                dir: info.dir.clone(),
                reason: reason.to_string(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000 * SECS_PER_DAY;
    const GIB: u64 = 1 << 30;

    fn profile(name: &str, gib: u64, built_days_ago: u64) -> ProfileInfo {
        ProfileInfo {
            dir: PathBuf::from(name),
            allocated_bytes: gib * GIB,
            last_built_unix: Some(NOW - built_days_ago * SECS_PER_DAY),
        }
    }

    fn names(chosen: &[(ProfileInfo, Reason)]) -> Vec<&str> {
        chosen
            .iter()
            .map(|(info, _)| info.dir.to_str().unwrap())
            .collect()
    }

    #[test]
    fn idle_rule_takes_only_old_enough_profiles() {
        let profiles = [
            profile("fresh", 1, 0),
            profile("week", 1, 7),
            profile("month", 1, 30),
        ];
        let limits = Limits {
            idle_days: Some(7),
            max_total_bytes: None,
        };

        let chosen = select(&profiles, NOW, limits);

        assert_eq!(names(&chosen), ["week", "month"]);
        assert_eq!(chosen[1].1, Reason::Idle { days: 30 });
    }

    #[test]
    fn cap_takes_the_least_recently_built_until_the_rest_fits() {
        let profiles = [profile("b", 4, 2), profile("a", 4, 3), profile("c", 4, 1)];
        let limits = Limits {
            idle_days: None,
            max_total_bytes: Some(5 * GIB),
        };

        let chosen = select(&profiles, NOW, limits);

        assert_eq!(names(&chosen), ["a", "b"]);
        let over_cap = Reason::OverCap {
            total_bytes: 8 * GIB,
            max_total_bytes: 5 * GIB,
        };
        assert_eq!(chosen[1].1, over_cap);
    }

    #[test]
    fn idle_evictions_count_toward_the_cap() {
        let profiles = [profile("old", 6, 40), profile("new", 4, 1)];
        let limits = Limits {
            idle_days: Some(30),
            max_total_bytes: Some(5 * GIB),
        };

        assert_eq!(names(&select(&profiles, NOW, limits)), ["old"]);
    }

    #[test]
    fn nothing_without_limits_under_the_cap_or_with_an_unknown_build_time() {
        let mut unknown = profile("unknown", 9, 0);
        unknown.last_built_unix = None;
        let profiles = [profile("a", 1, 400), unknown];

        assert!(select(&profiles, NOW, Limits::default()).is_empty());
        let roomy = Limits {
            idle_days: None,
            max_total_bytes: Some(10 * GIB),
        };
        assert!(select(&profiles, NOW, roomy).is_empty());
        let tight = Limits {
            idle_days: Some(1),
            max_total_bytes: Some(0),
        };
        assert_eq!(names(&select(&profiles, NOW, tight)), ["a"]);
    }
}
