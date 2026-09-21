//! The lossy `orphans` pass: whole target dirs of projects that are git worktrees their
//! repository no longer registers. Nothing else in such a checkout is touched — the sources may
//! hold uncommitted work that git can no longer report, and only `target/` is rebuildable.

use std::path::{Path, PathBuf};

use crate::engine::{Action, Pass};
use crate::inventory;
use crate::model::Profile;

pub const NAME: &str = "orphans";
const REASON: &str = "git no longer has a worktree record for this checkout";

/// One orphaned target, as the inventory found it before any lock was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphan {
    pub target: PathBuf,
    /// The project the target was built from: the one whose worktree record is gone.
    pub project: PathBuf,
    /// What `du` reports for the whole target dir, for the report.
    pub allocated_bytes: u64,
}

pub struct Orphans {
    chosen: Vec<Orphan>,
}

impl Orphans {
    pub fn new(chosen: Vec<Orphan>) -> Self {
        Self { chosen }
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
        let inside = |target: &Path| {
            profiles
                .iter()
                .any(|profile| profile.dir.starts_with(target))
        };
        self.chosen
            .iter()
            // The engine holds a lock inside the target, and git still says the record is gone:
            // a worktree re-registered since the inventory keeps everything it built.
            .filter(|orphan| inside(&orphan.target) && inventory::is_orphaned(&orphan.project))
            .map(|orphan| Action::RemoveTarget {
                target: orphan.target.clone(),
                dir: orphan.target.clone(),
                reason: REASON.to_string(),
                bytes: orphan.allocated_bytes,
            })
            .collect()
    }
}
