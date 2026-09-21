//! The lossy `orphans` pass on real `git worktree` checkouts in temp dirs. Nothing here points
//! at a real repository or a real target.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

use dunnage::engine::{self, Locks, Options, Pass, Report};
use dunnage::inventory;
use dunnage::model::CARGO_LOCK_FILE;
use dunnage::orphans::{self, Orphan, Orphans};
use predicates::str::contains;
use tempfile::TempDir;

mod common;
use common::{allocated_bytes, dunnage as dunnage_in, fake_target, run_unbusy};

const KIB: usize = 1024;
const PROFILE_KIB: usize = 64;

fn root() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    (tmp, root)
}

fn git(dir: &Path, args: &[&str]) {
    let status = Process::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=dunnage",
            "-c",
            "user.email=dunnage@invalid",
        ])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository and one worktree of it, each with a target dir: one family, one of them a
/// worktree whose record can be taken away. Returns the two profile dirs.
fn family(root: &Path) -> (PathBuf, PathBuf) {
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("src.rs"), b"// committed\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    git(&repo, &["worktree", "add", "../wt"]);
    // Uncommitted work git can no longer report once the record is gone: the reason the pass
    // removes `target/` only and leaves the checkout alone.
    fs::write(root.join("wt/uncommitted.rs"), b"// not in any commit\n").unwrap();
    (
        fake_target(root, "repo", PROFILE_KIB, 1),
        fake_target(root, "wt", PROFILE_KIB, 1),
    )
}

fn record(root: &Path) -> PathBuf {
    root.join("repo/.git/worktrees/wt")
}

/// What a deleted repository clone or a stray `rm -rf` leaves behind: the checkout and its
/// `.git` file, pointing at a record that is gone.
fn orphan(root: &Path) {
    fs::rename(record(root), root.join("record-stash")).unwrap();
}

fn named() -> Options {
    Options {
        dry_run: false,
        lossy: vec![orphans::NAME.to_string()],
    }
}

/// What the CLI does: inventory of `root`, the orphans it found, one engine run over every
/// profile dir. All of it again on a retry, since a removed target cannot be locked.
fn run(root: &Path, opts: &Options) -> Report {
    run_unbusy(|| {
        let inventory = inventory::inventory(&[root.to_path_buf()]).unwrap();
        engine::run(
            &profile_dirs(&inventory),
            &[&chosen(&inventory)],
            opts,
            Locks::PerDir,
        )
        .unwrap()
    })
}

fn chosen(inventory: &inventory::Inventory) -> Orphans {
    Orphans::new(
        inventory
            .targets
            .iter()
            .filter(|target| target.orphaned)
            .map(|target| Orphan {
                target: target.root.clone(),
                allocated_bytes: target.allocated_bytes,
            })
            .collect(),
    )
}

fn profile_dirs(inventory: &inventory::Inventory) -> Vec<PathBuf> {
    inventory
        .targets
        .iter()
        .flat_map(|target| target.profiles.iter().map(|profile| profile.dir.clone()))
        .collect()
}

#[test]
fn an_orphaned_target_goes_whole_and_the_checkout_stays() {
    let (_tmp, root) = root();
    let (repo, wt) = family(&root);
    let target = wt.parent().unwrap().to_path_buf();
    // Everything a target holds outside its profile dirs goes with it.
    fs::create_dir_all(target.join("doc")).unwrap();
    fs::write(target.join("doc/index.html"), b"<!-- docs -->").unwrap();
    orphan(&root);

    let report = run(&root, &named());

    let pass = &report.passes[0];
    assert_eq!((pass.planned, pass.applied), (1, 1), "{pass:?}");
    assert!(pass.freed_bytes >= (PROFILE_KIB * KIB) as u64);
    assert_eq!(pass.removals[0].0, target);
    assert!(pass.removals[0].1.contains("worktree record"), "{pass:?}");
    assert!(!target.exists());
    // The checkout itself, including work no commit holds, and the repository's own target.
    assert!(root.join("wt/uncommitted.rs").exists());
    assert!(root.join("wt/.git").exists());
    assert!(repo.join("deps/libx.rlib").exists());
}

#[test]
fn a_live_worktree_is_untouched() {
    let (_tmp, root) = root();
    let (repo, wt) = family(&root);

    let report = run(&root, &named());

    assert_eq!(report.passes[0].planned, 0, "{report:?}");
    assert!(wt.join("deps/libx.rlib").exists());
    assert!(repo.join("deps/libx.rlib").exists());
}

#[test]
fn a_target_with_a_running_build_is_not_touched() {
    let (_tmp, root) = root();
    let (_repo, wt) = family(&root);
    orphan(&root);
    // What cargo holds for the length of a build.
    let build = File::options()
        .write(true)
        .open(wt.join(CARGO_LOCK_FILE))
        .unwrap();
    build.lock().unwrap();

    let report = run(&root, &named());

    assert_eq!(report.busy, std::slice::from_ref(&wt));
    assert_eq!(report.passes[0].planned, 0, "{report:?}");
    assert!(wt.join("deps/libx.rlib").exists());
}

#[test]
fn a_worktree_registered_again_after_the_inventory_is_kept() {
    let (_tmp, root) = root();
    let (_repo, wt) = family(&root);
    orphan(&root);
    let inventory = inventory::inventory(std::slice::from_ref(&root)).unwrap();
    let pass = chosen(&inventory);
    // A target none of whose profile dirs we hold is never planned, orphan or not.
    assert_eq!(pass.plan(&[]).len(), 0);

    // `git worktree repair` runs between the inventory and our lock.
    fs::rename(root.join("record-stash"), record(&root)).unwrap();
    let dirs = profile_dirs(&inventory);
    let report = run_unbusy(|| engine::run(&dirs, &[&pass], &named(), Locks::PerDir).unwrap());

    assert_eq!(report.passes[0].planned, 0, "{report:?}");
    assert!(wt.join("deps/libx.rlib").exists());
}

#[test]
fn nothing_happens_unless_the_pass_is_named_and_a_dry_run_only_lists() {
    let (_tmp, root) = root();
    let (_repo, wt) = family(&root);
    orphan(&root);

    let unnamed = run(&root, &Options::default());
    assert!(unnamed.passes.is_empty(), "{unnamed:?}");

    let dry = Options {
        dry_run: true,
        ..named()
    };
    let listed = run(&root, &dry);
    let pass = &listed.passes[0];
    assert_eq!((pass.planned, pass.applied), (1, 0), "{pass:?}");
    assert_eq!(pass.removals.len(), 1);
    assert!(pass.planned_bytes >= (PROFILE_KIB * KIB) as u64);
    assert!(wt.join("deps/libx.rlib").exists());
}

/// A/B: the same tree twice, the pass the only difference. What the control keeps is what the
/// treatment frees, and neither one touches anything outside `target/`.
#[test]
fn ab_only_the_named_run_frees_the_orphan_and_neither_run_touches_sources() {
    let (_tmp_a, control) = root();
    let (_tmp_b, treatment) = root();
    for root in [&control, &treatment] {
        family(root);
        orphan(root);
    }
    let before = allocated_bytes(&control.join("wt"));
    assert_eq!(before, allocated_bytes(&treatment.join("wt")));

    run(&control, &Options::default());
    run(&treatment, &named());

    let control_after = allocated_bytes(&control.join("wt"));
    let treatment_after = allocated_bytes(&treatment.join("wt"));
    assert_eq!(control_after, before, "the control must not lose bytes");
    assert!(
        before - treatment_after >= (PROFILE_KIB * KIB) as u64,
        "{before} -> {treatment_after}"
    );
    // The difference is the target dir and nothing else.
    assert!(!treatment.join("wt/target").exists());
    assert_eq!(
        control_after - treatment_after,
        allocated_bytes(&control.join("wt/target"))
    );
    for root in [&control, &treatment] {
        assert!(root.join("wt/uncommitted.rs").exists());
        assert!(root.join("repo/target/debug/deps/libx.rlib").exists());
    }
}

#[test]
fn cli_removes_an_orphan_only_when_asked() {
    let (_tmp, root) = root();
    let (_repo, wt) = family(&root);
    orphan(&root);
    let index = root.join("index.bin");
    let dunnage = || {
        let mut cmd = dunnage_in(&root);
        cmd.args(["run", "--pass", "orphans", "--index"]);
        cmd.arg(&index);
        cmd
    };

    dunnage().arg(&root).assert().success();
    assert!(wt.exists(), "a run that does not name the pass keeps it");

    dunnage()
        .args(["--dry-run", "--lossy", "orphans"])
        .arg(&root)
        .assert()
        .success()
        .stdout(contains("would remove"))
        .stdout(contains("worktree record"));
    assert!(wt.exists());

    dunnage()
        .args(["--lossy", "orphans"])
        .arg(&root)
        .assert()
        .success()
        .stdout(contains("orphans: planned 1"));
    assert!(!wt.parent().unwrap().exists());
    assert!(root.join("wt/uncommitted.rs").exists());
}
