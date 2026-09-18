//! The lossy `doc` pass: `<target>/doc`, which `cargo doc` writes from scratch every time and
//! no build reads. `cargo clean --doc` removes the same dir; the profile dirs stay untouched,
//! so nothing in the build graph goes stale.

use std::path::{Path, PathBuf};

use crate::engine::{Action, Pass};
use crate::model::Profile;

pub const NAME: &str = "doc";
/// Cargo's own name for the dir `cargo doc` writes into, directly under the target.
pub const DIR: &str = "doc";
const REASON: &str = "rustdoc output, which `cargo doc` writes again from scratch";

/// One target's `doc/`, as the inventory found it before any lock was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Docs {
    pub target: PathBuf,
    /// What `du` reports for `<target>/doc`, for the report.
    pub allocated_bytes: u64,
}

impl Docs {
    fn dir(&self) -> PathBuf {
        self.target.join(DIR)
    }
}

pub struct Doc {
    chosen: Vec<Docs>,
}

impl Doc {
    pub fn new(chosen: Vec<Docs>) -> Self {
        Self { chosen }
    }
}

impl Pass for Doc {
    fn name(&self) -> &'static str {
        NAME
    }

    fn lossy(&self) -> bool {
        true
    }

    fn plan(&self, profiles: &[Profile]) -> Vec<Action> {
        // `doc/` sits beside the profile dirs, not inside one, so what guards it is the target's
        // own build locks: the engine checks them for us.
        let inside = |target: &Path| {
            profiles
                .iter()
                .any(|profile| profile.dir.starts_with(target))
        };
        self.chosen
            .iter()
            .filter(|docs| inside(&docs.target) && docs.dir().is_dir())
            .map(|docs| Action::RemoveTarget {
                target: docs.target.clone(),
                dir: docs.dir(),
                reason: REASON.to_string(),
                bytes: docs.allocated_bytes,
            })
            .collect()
    }
}
