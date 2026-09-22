//! A content-addressed store the user names: `GOCACHE`, `~/.cabal/store`, Zig's `o/`, dune's
//! shared cache. Files there live under names derived from their content and never get other
//! bytes, so `compress` needs no lock — only the age floor that keeps it off an entry still
//! being written. Nothing is discovered: a store is a run of its own, named by `--store`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Ecosystem, Guard, Policy, REGISTRY, Sharing};
use crate::eco::cargo::home;

/// The adapter of every named store.
pub struct Store;

pub static STORE: Store = Store;

/// Caches that compress their own entries: a second compression wins nothing and costs a
/// rewrite of every entry. Their default dir names, for the case where no marker file is found.
const SELF_COMPRESSING: &[&str] = &["ccache", "sccache", "Mozilla.sccache"];

impl Ecosystem for Store {
    fn name(&self) -> &'static str {
        "store"
    }

    /// The whole store is one unit: there is no build that works on a part of it.
    fn units(&self, build_dir: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(vec![build_dir.to_path_buf()])
    }

    fn guard(&self, _unit: &Path) -> Guard {
        Guard::Immutable
    }

    /// Nothing is shared: dedupe finds nothing in a content-addressed store by construction.
    fn policy(&self) -> Policy {
        Policy {
            share: Sharing::ClonesOnly,
        }
    }
}

/// Why `dir` is not a store this tool may compress, or `None` when it may. Refused: what is not
/// a dir, a self-compressing cache (ccache, sccache), and a dir inside a build dir some adapter
/// claims or inside a cargo home, whose files do get new bytes under old names. A dir that holds
/// a build dir somewhere below is not looked for: that would take a walk of the whole store.
pub fn check(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return Some("not a directory".into());
    }
    let tag = fs::read_to_string(dir.join("CACHEDIR.TAG")).unwrap_or_default();
    let named = dir
        .file_name()
        .is_some_and(|name| SELF_COMPRESSING.iter().any(|known| name == *known));
    if named || dir.join("ccache.conf").exists() || tag.contains("ccache") {
        return Some("a ccache or sccache dir: it compresses its own entries".into());
    }
    for up in dir.ancestors() {
        if let Some(eco) = REGISTRY.iter().find(|eco| eco.claim(up)) {
            return Some(format!(
                "inside a {} build dir ({}), whose files are rewritten in place",
                eco.name(),
                up.display()
            ));
        }
        if up.join(home::LOCK_FILE).exists() {
            return Some(format!(
                "inside a cargo home ({}): use --cargo-home for it",
                up.display()
            ));
        }
    }
    None
}
