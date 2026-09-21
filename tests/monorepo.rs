//! One repository with several workspaces at different positions, and a worktree of it: what
//! the adapter boundary already gets right. Fake targets in temp dirs only.

use std::fs;
use std::path::{Path, PathBuf};

use dunnage::eco::cargo::CARGO;
use dunnage::inventory;
use dunnage::seed;
use dunnage::session::{Session, Settings};
use tempfile::TempDir;

mod common;
use common::{fake_target, git};

/// `repo` with workspaces `services/api` and `tools/cli`, both built, and a worktree `wt` with
/// only `services/api` built. A build script of `api` left a CMake build dir and a whole nested
/// cargo target inside `api`'s target.
struct Mono {
    _tmp: TempDir,
    repo: PathBuf,
    wt: PathBuf,
}

impl Mono {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let (repo, wt) = (base.join("repo"), base.join("wt"));
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("README"), "mono\n").unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "mono"]);
        git(&repo, &["worktree", "add", wt.to_str().unwrap()]);

        fake_target(&repo, "services/api", 16, 0);
        fake_target(&repo, "tools/cli", 16, 0);
        fake_target(&wt, "services/api", 16, 0);
        let out = repo.join("services/api/target/debug/build/sys-1/out");
        fs::create_dir_all(out.join("build")).unwrap();
        fs::write(
            out.join("build/CMakeCache.txt"),
            "CMAKE_HOME_DIRECTORY:INTERNAL=x\n",
        )
        .unwrap();
        fake_target(&out, "vendored", 16, 0);
        fs::create_dir_all(wt.join("tools/cli")).unwrap();
        Self {
            _tmp: tmp,
            repo,
            wt,
        }
    }
}

/// The build dirs found, sorted by path.
fn roots(targets: &[inventory::Target]) -> Vec<&Path> {
    let mut roots: Vec<&Path> = targets.iter().map(|target| target.root.as_path()).collect();
    roots.sort();
    roots
}

#[test]
fn every_build_dir_is_found_once_and_a_nested_one_is_nobodys() {
    let mono = Mono::new();
    let base = mono.repo.parent().unwrap().to_path_buf();

    // Roots that overlap still find each dir once.
    let found = inventory::inventory(&[base.clone(), mono.repo.clone()]).unwrap();

    assert_eq!(
        roots(&found.targets),
        [
            mono.repo.join("services/api/target"),
            mono.repo.join("tools/cli/target"),
            mono.wt.join("services/api/target"),
        ]
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>()
    );
    for target in &found.targets {
        assert!(
            !target
                .root
                .starts_with(mono.repo.join("services/api/target/debug")),
            "the nested target is part of api's build, not a build dir of its own"
        );
    }
}

#[test]
fn every_position_of_every_checkout_is_one_family_and_owned_by_its_workspace() {
    let mono = Mono::new();
    let base = mono.repo.parent().unwrap().to_path_buf();

    let found = inventory::inventory(&[base]).unwrap();

    let common = mono.repo.join(".git");
    for target in &found.targets {
        assert_eq!(
            target.family.as_deref(),
            Some(common.as_path()),
            "{target:?}"
        );
        assert!(!target.orphaned);
        assert_eq!(target.project.as_deref(), target.root.parent());
    }
}

#[test]
fn seed_finds_the_same_position_in_a_sibling_checkout() {
    let mono = Mono::new();

    assert_eq!(
        seed::choose(&mono.wt.join("tools/cli"), &CARGO),
        Some(mono.repo.join("tools/cli/target"))
    );
    // `services/api` of the main checkout has the worktree's as its sibling.
    assert_eq!(
        seed::choose(&mono.repo.join("services/api"), &CARGO),
        Some(mono.wt.join("services/api/target"))
    );
}

/// Moves the entries of every profile dir of `target` back by `days`: what `last_used` reads.
fn built_days_ago(target: &Path, days: u64) {
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 24 * 60 * 60);
    for profile in dunnage::eco::cargo::profile_dirs(target).unwrap() {
        for entry in fs::read_dir(&profile).unwrap() {
            let entry = fs::File::open(entry.unwrap().path()).unwrap();
            entry.set_modified(then).unwrap();
        }
    }
}

#[test]
fn seed_fills_every_position_from_the_sibling_that_built_it_last() {
    let mono = Mono::new();
    // `api` was built last in the worktree, `cli` only ever in the main checkout, and `legacy`
    // exists in the main checkout only: absent on the new branch.
    built_days_ago(&mono.repo.join("services/api/target"), 5);
    fake_target(&mono.repo, "tools/legacy", 16, 0);
    let fresh = mono.repo.parent().unwrap().join("fresh");
    git(&mono.repo, &["worktree", "add", fresh.to_str().unwrap()]);
    for project in ["services/api", "tools/cli"] {
        fs::create_dir_all(fresh.join(project)).unwrap();
    }

    let chosen: Vec<(PathBuf, PathBuf)> = seed::positions(&fresh)
        .into_iter()
        .map(|position| (position.project, position.source))
        .collect();
    assert_eq!(
        chosen,
        [
            (
                fresh.join("services/api"),
                mono.wt.join("services/api/target")
            ),
            (fresh.join("tools/cli"), mono.repo.join("tools/cli/target")),
        ]
    );

    let state = mono.repo.parent().unwrap().join("state");
    let session = Session::open(Settings {
        index: state.join("hashes.bin"),
        ..Settings::default()
    });
    let done = session.seed(&fresh, None, false).unwrap();

    assert_eq!(done.len(), 2);
    for project in ["services/api", "tools/cli"] {
        assert!(
            fresh
                .join(project)
                .join("target/debug/deps/libx.rlib")
                .is_file()
        );
    }
    assert!(!fresh.join("tools/legacy").exists());
    // Everything is filled now: a second seed has nothing to do and says so.
    assert!(session.seed(&fresh, None, false).is_err());
}
