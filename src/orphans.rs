//! The lossy `orphans` pass: whole build dirs whose reason to exist is gone. Two reasons:
//!
//! - the checkout is a git worktree its repository no longer registers, or is gone altogether
//!   while a build dir it had moved out of it is still there;
//! - the project's manifest is gone — deleted, renamed, or absent on this branch. A branch switch
//!   looks exactly like a deletion, so this one only counts once the build dir has been idle for
//!   the days the caller asks for.
//!
//! Nothing else in such a checkout is touched — the sources may hold uncommitted work that git
//! can no longer report, and only the build dir is rebuildable.

use std::fs;
use std::path::{Path, PathBuf};

use crate::engine::{Action, Pass};
use crate::inventory;
use crate::model::Profile;

pub const NAME: &str = "orphans";
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// Why a build dir has no reason left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Git no longer has a worktree record for the checkout, or the checkout is gone.
    CheckoutGone,
    /// `manifest` does not exist, and the build dir was not used for `idle_days`.
    ProjectGone { manifest: PathBuf, idle_days: u64 },
}

impl Reason {
    fn text(&self) -> String {
        match self {
            Self::CheckoutGone => {
                "the checkout is gone: git has no worktree record for it, or its dir is gone".into()
            }
            Self::ProjectGone {
                manifest,
                idle_days,
            } => format!(
                "{} is gone and nothing was built here for {idle_days} days",
                manifest.display()
            ),
        }
    }
}

/// One orphaned build dir, as the inventory found it before any lock was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphan {
    pub target: PathBuf,
    /// The project the target was built from.
    pub project: PathBuf,
    /// What `du` reports for the whole target dir, for the report.
    pub allocated_bytes: u64,
    pub reason: Reason,
}

pub struct Orphans {
    chosen: Vec<Orphan>,
    now_unix: u64,
}

impl Orphans {
    /// `now_unix` is what the idle days of [`Reason::ProjectGone`] are counted back from.
    pub fn new(chosen: Vec<Orphan>, now_unix: u64) -> Self {
        Self { chosen, now_unix }
    }
}

impl Pass for Orphans {
    fn name(&self) -> &'static str {
        NAME
    }

    fn lossy(&self) -> bool {
        true
    }

    fn plan(&self, profiles: &[Profile]) -> Vec<Action> {
        // Under the lock, the reason is checked once more: a worktree re-registered, a manifest
        // back after a branch switch, or a build since the inventory keeps everything.
        let holds = |orphan: &Orphan| match &orphan.reason {
            Reason::CheckoutGone => inventory::is_orphaned(&orphan.project),
            Reason::ProjectGone {
                manifest,
                idle_days,
            } => {
                let last = inside(profiles, &orphan.target)
                    .map(|p| p.last_used)
                    .max()
                    .flatten();
                // Gone, not merely unreadable: a permission or I/O error keeps the target.
                fs::symlink_metadata(manifest)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                    && last.is_some_and(|built| {
                        self.now_unix.saturating_sub(built)
                            >= idle_days.saturating_mul(SECS_PER_DAY)
                    })
            }
        };
        self.chosen
            .iter()
            .filter(|orphan| inside(profiles, &orphan.target).next().is_some() && holds(orphan))
            .map(|orphan| Action::RemoveTarget {
                target: orphan.target.clone(),
                dir: orphan.target.clone(),
                reason: orphan.reason.text(),
                bytes: orphan.allocated_bytes,
            })
            .collect()
    }
}

/// The profiles of `profiles` inside `target`: the engine holds their locks.
fn inside<'a>(profiles: &'a [Profile], target: &'a Path) -> impl Iterator<Item = &'a Profile> {
    profiles
        .iter()
        .filter(move |profile| profile.dir.starts_with(target))
}
