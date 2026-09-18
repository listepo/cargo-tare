//! One test per safety invariant of `DESIGN.md`, on throwaway dirs only.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use cargo_tare::engine::{self, Action, Options, Pass, Replace, Report, Skip};
use cargo_tare::model::{self, CARGO_LOCK_FILE, Profile, TMP_PREFIX};
use tempfile::TempDir;

mod common;
use common::{BIN, BUILD_SLEEP_ENV, Fixture, POLL, ino, run_unbusy};

const CONTENT: &[u8] = b"same bytes in both files";
const MEMBER_MODE: u32 = 0o640;
const OLD_MTIME: Duration = Duration::from_secs(1_000_000_000);

struct FnPass<F> {
    lossy: bool,
    plan: F,
}

impl<F: Fn(&[Profile]) -> Vec<Action>> Pass for FnPass<F> {
    fn name(&self) -> &'static str {
        "test"
    }
    fn lossy(&self) -> bool {
        self.lossy
    }
    fn plan(&self, profiles: &[Profile]) -> Vec<Action> {
        (self.plan)(profiles)
    }
}

/// Plans: replace the inode of file `member` with a clone of file `source`, if both were scanned.
fn replace_by_name(profiles: &[Profile], source: &str, member: &str) -> Vec<Action> {
    let find = |name: &str| {
        profiles
            .iter()
            .flat_map(|p| &p.inodes)
            .find(|i| i.paths.iter().any(|p| p.file_name().unwrap() == name))
    };
    let (Some(source), Some(member)) = (find(source), find(member)) else {
        return Vec::new();
    };
    vec![Action::Replace(Replace {
        source: source.paths[0].clone(),
        source_stamp: source.stamp.clone(),
        member: member.clone(),
    })]
}

fn run(profile: &Path, passes: &[&dyn Pass], opts: &Options) -> Report {
    run_unbusy(|| engine::run(&[profile.to_path_buf()], passes, opts).unwrap())
}

fn run_replace(profile: &Path, opts: &Options) -> Report {
    let pass = FnPass {
        lossy: false,
        plan: |p: &[Profile]| replace_by_name(p, "canon", "member"),
    };
    run(profile, &[&pass], opts)
}

/// A profile dir with `canon` and `member` (same bytes, different inodes); `member` is old.
fn profile() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().canonicalize().unwrap().join("debug");
    fs::create_dir_all(dir.join("deps")).unwrap();
    File::create(dir.join(CARGO_LOCK_FILE)).unwrap();
    fs::write(dir.join("canon"), CONTENT).unwrap();
    let member = dir.join("deps/member");
    fs::write(&member, CONTENT).unwrap();
    fs::set_permissions(&member, fs::Permissions::from_mode(MEMBER_MODE)).unwrap();
    let mtime = SystemTime::UNIX_EPOCH + OLD_MTIME;
    File::open(&member).unwrap().set_modified(mtime).unwrap();
    (tmp, dir)
}

fn only_skip(report: &Report) -> &Skip {
    assert_eq!(report.passes[0].applied, 0);
    &report.passes[0].skipped[0].1
}

#[test]
fn replaces_whole_hardlink_group_and_keeps_mtime_and_mode() {
    let (_tmp, dir) = profile();
    let member = dir.join("deps/member");
    let link = dir.join("member-link");
    fs::hard_link(&member, &link).unwrap();
    let old_ino = ino(&member);

    let report = run_replace(&dir, &Options::default());

    assert_eq!(report.passes[0].applied, 1, "{report:?}");
    assert_ne!(ino(&member), old_ino);
    assert_eq!(ino(&member), ino(&link), "the group must stay one inode");
    assert_ne!(
        ino(&member),
        ino(&dir.join("canon")),
        "a clone, not a hardlink"
    );
    let meta = fs::metadata(&member).unwrap();
    assert_eq!(meta.modified().unwrap(), SystemTime::UNIX_EPOCH + OLD_MTIME);
    assert_eq!(meta.mode() & 0o7777, MEMBER_MODE);
    assert_eq!(fs::read(&member).unwrap(), CONTENT);
    assert!(model::scan(&dir).unwrap().stale_temps.is_empty());
}

#[test]
fn busy_profile_is_skipped_untouched() {
    let (_tmp, dir) = profile();
    let stale = dir.join(format!("{TMP_PREFIX}crashed"));
    fs::write(&stale, b"x").unwrap();
    let held = File::open(dir.join(CARGO_LOCK_FILE)).unwrap();
    held.lock().unwrap();
    let old_ino = ino(&dir.join("deps/member"));

    let report = run_replace(&dir, &Options::default());

    assert_eq!(report.busy, [dir.as_path()]);
    assert_eq!(report.passes[0].planned, 0);
    assert_eq!(ino(&dir.join("deps/member")), old_ino);
    assert!(stale.exists());
}

#[test]
fn stale_temps_are_removed_but_not_on_dry_run() {
    let (_tmp, dir) = profile();
    let stale = dir.join("deps").join(format!("{TMP_PREFIX}crashed"));
    fs::write(&stale, b"x").unwrap();
    let dry = Options {
        dry_run: true,
        ..Options::default()
    };

    assert_eq!(run(&dir, &[], &dry).temps_removed, 0);
    assert!(stale.exists());
    let report = run(&dir, &[], &Options::default());
    assert_eq!(report.temps_removed, 1, "{report:?}");
    assert!(!stale.exists());
}

#[test]
fn dry_run_plans_but_applies_nothing() {
    let (_tmp, dir) = profile();
    let old_ino = ino(&dir.join("deps/member"));
    let dry = Options {
        dry_run: true,
        ..Options::default()
    };

    let report = run_replace(&dir, &dry);

    assert_eq!((report.passes[0].planned, report.passes[0].applied), (1, 0));
    assert_eq!(ino(&dir.join("deps/member")), old_ino);
}

#[test]
fn member_changed_after_the_scan_is_skipped() {
    let (_tmp, dir) = profile();
    let member = dir.join("deps/member");
    let pass = FnPass {
        lossy: false,
        plan: |p: &[Profile]| {
            let actions = replace_by_name(p, "canon", "member");
            // rustc rewrites the file between plan and apply
            let mut file = File::options().append(true).open(&member).unwrap();
            file.write_all(b"!").unwrap();
            actions
        },
    };

    let report = run(&dir, &[&pass], &Options::default());

    assert_eq!(only_skip(&report), &Skip::Changed);
    assert!(fs::read(&member).unwrap().ends_with(b"!"));
}

#[test]
fn group_with_a_link_outside_the_profile_is_skipped() {
    let (tmp, dir) = profile();
    fs::hard_link(dir.join("deps/member"), tmp.path().join("outside")).unwrap();

    assert_eq!(
        only_skip(&run_replace(&dir, &Options::default())),
        &Skip::ForeignLinks
    );
}

#[cfg(target_os = "macos")]
#[test]
fn flagged_member_is_skipped() {
    let (_tmp, dir) = profile();
    let status = Command::new("chflags")
        .arg("nodump")
        .arg(dir.join("deps/member"))
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(
        only_skip(&run_replace(&dir, &Options::default())),
        &Skip::Flags
    );
}

#[test]
fn paths_outside_locked_profiles_are_refused() {
    let (tmp, dir) = profile();
    let outside = tmp.path().canonicalize().unwrap().join("outside");
    fs::write(&outside, CONTENT).unwrap();
    let pass = FnPass {
        lossy: false,
        plan: |p: &[Profile]| {
            let mut actions = replace_by_name(p, "canon", "member");
            if let Action::Replace(replace) = &mut actions[0] {
                replace.member.paths = vec![outside.clone()];
            }
            actions
        },
    };

    let report = run(&dir, &[&pass], &Options::default());

    assert_eq!(only_skip(&report), &Skip::Unlocked);
}

#[test]
fn lossy_pass_runs_only_when_named() {
    let (_tmp, dir) = profile();
    let pass = FnPass {
        lossy: true,
        plan: |p: &[Profile]| replace_by_name(p, "canon", "member"),
    };
    let enabled = Options {
        lossy: vec!["test".into()],
        ..Options::default()
    };

    let off = run(&dir, &[&pass], &Options::default());
    let on = run(&dir, &[&pass], &enabled);

    assert!(off.passes.is_empty());
    assert_eq!(on.passes[0].applied, 1);
}

#[test]
fn scan_does_not_follow_symlinks() {
    let (tmp, dir) = profile();
    let outside = tmp.path().join("outside-dir");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"x").unwrap();
    symlink(&outside, dir.join("link-dir")).unwrap();
    symlink(outside.join("secret"), dir.join("link-file")).unwrap();

    let scan = model::scan(&dir).unwrap();

    let names: Vec<_> = scan
        .inodes
        .iter()
        .flat_map(|i| &i.paths)
        .map(|p| p.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, ["canon", "member"]);
}

#[test]
fn profile_dirs_refuses_a_dir_cargo_did_not_tag() {
    let (_tmp, dir) = profile();
    let target = dir.parent().unwrap();
    assert!(model::profile_dirs(target).is_err(), "no tag at all");
    fs::write(
        target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55",
    )
    .unwrap();
    assert!(model::profile_dirs(target).is_err(), "someone else's tag");
    fs::write(target.join("CACHEDIR.TAG"), "# tag created by cargo.").unwrap();
    assert_eq!(model::profile_dirs(target).unwrap(), [dir.as_path()]);
}

// --- a real cargo build ---

const BUILD_SCRIPT_SLEEP_SECS: &str = "3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(120);

#[test]
fn running_build_is_not_disturbed_and_replaced_artifacts_stay_fresh() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let profile = target.join("debug");

    // Invariant 1 against the real thing: while cargo builds, the profile reads as busy.
    let mut build = fixture
        .cargo(&target, &["build"])
        .env(BUILD_SLEEP_ENV, BUILD_SCRIPT_SLEEP_SECS)
        .spawn()
        .unwrap();
    let started = Instant::now();
    let mut seen_busy = false;
    while !seen_busy && build.try_wait().unwrap().is_none() {
        assert!(started.elapsed() < BUSY_TIMEOUT);
        // Err: cargo has not created the lock file yet.
        if let Ok(report) = engine::run(std::slice::from_ref(&profile), &[], &Options::default()) {
            seen_busy = !report.busy.is_empty();
        }
        std::thread::sleep(POLL);
    }
    assert!(build.wait().unwrap().success());
    assert!(seen_busy, "never saw cargo holding the lock");
    // Cargo's real tag and lock file are what `profile_dirs` expects.
    assert_eq!(model::profile_dirs(&target).unwrap(), [profile.as_path()]);
    fixture.build(&target);

    // Freshness: replace the final binary's hardlink group (`fx` and `deps/fx-<hash>`).
    fs::copy(profile.join(BIN), profile.join("canon")).unwrap();
    let old_ino = ino(&profile.join(BIN));
    let pass = FnPass {
        lossy: false,
        plan: |p: &[Profile]| replace_by_name(p, "canon", BIN),
    };
    let report = run(&profile, &[&pass], &Options::default());
    assert_eq!(report.passes[0].applied, 1, "{report:?}");
    assert_ne!(ino(&profile.join(BIN)), old_ino);

    fixture.assert_fresh(&target);
}
