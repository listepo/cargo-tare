//! `dunnage worktree add` on a small git repository. Everything is in temp dirs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use predicates::str::contains;
use tempfile::TempDir;

mod common;
use common::{dunnage, stale_units_at};

const EXIT_FAILURE: i32 = 1;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
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

/// A workspace with one third-party dependency, committed as a repository and built once in
/// place. The dependency is a vendored directory source *outside* the repository, the way
/// registry crates sit in `~/.cargo` and not in a checkout: that is what a new worktree can reuse.
/// Path dependencies move with the worktree and are rebuilt whatever the target holds.
struct Repo {
    tmp: TempDir,
    root: PathBuf,
}

const MANIFEST: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                        [dependencies]\nthird = \"1.0.0\"\n";
const THIRD: &str = "[package]\nname = \"third\"\nversion = \"1.0.0\"\nedition = \"2021\"\n";

impl Repo {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let vendor = base.join("shared/vendor/third");
        let ws = base.join("repo/ws");
        for (path, content) in [
            (vendor.join("Cargo.toml"), THIRD.to_string()),
            (
                vendor.join("src/lib.rs"),
                "pub fn n() -> u32 { 7 }\n".into(),
            ),
            (
                vendor.join(".cargo-checksum.json"),
                r#"{"files":{},"package":null}"#.into(),
            ),
            (ws.join("Cargo.toml"), MANIFEST.into()),
            (
                ws.join("src/main.rs"),
                "fn main() { assert_eq!(third::n(), 7); }\n".into(),
            ),
            (
                ws.join(".cargo/config.toml"),
                format!(
                    "[source.crates-io]\nreplace-with = \"vendored\"\n\n\
                     [source.vendored]\ndirectory = {:?}\n",
                    vendor.parent().unwrap()
                ),
            ),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        let root = base.join("repo");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let lockfile = Command::new(cargo)
            .current_dir(&ws)
            .args(["generate-lockfile", "--offline"])
            .output()
            .unwrap();
        assert!(
            lockfile.status.success(),
            "{}",
            String::from_utf8_lossy(&lockfile.stderr)
        );
        git(&root, &["init", "-b", "main"]);
        // Committed before anything is built, so no target dir can reach a worktree.
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "fixture"]);
        assert!(
            stale_units_at(&ws, &ws.join("target")).len() > 1,
            "built from scratch"
        );
        Self { tmp, root }
    }

    fn target(&self) -> PathBuf {
        self.root.join("ws/target")
    }

    /// Where a worktree called `name` goes.
    fn worktree(&self, name: &str) -> PathBuf {
        self.tmp.path().canonicalize().unwrap().join(name)
    }

    /// `dunnage worktree add`, run from the workspace — a subdir of the checkout, so the new
    /// worktree is seeded at `<worktree>/ws`.
    fn add(&self, extra: &[&str], git_args: &[&str]) -> assert_cmd::assert::Assert {
        let state = self.tmp.path().join("state");
        dunnage(&state)
            .current_dir(self.root.join("ws"))
            .args(["worktree", "add", "--index"])
            .arg(state.join("hashes.bin"))
            .args(extra)
            .args(git_args)
            .assert()
    }
}

/// Units cargo rebuilds in `worktree`'s workspace, by name.
fn rebuilt(worktree: &Path) -> Vec<String> {
    let ws = worktree.join("ws");
    stale_units_at(&ws, &ws.join("target"))
}

#[test]
fn a_worktree_added_by_the_tool_starts_with_its_third_party_units_fresh() {
    let repo = Repo::new();
    let (seeded, plain) = (repo.worktree("seeded"), repo.worktree("plain"));

    repo.add(&[], &[seeded.to_str().unwrap(), "-b", "seeded"])
        .success()
        .stdout(contains("added worktree"))
        .stdout(contains("copied"));
    git(
        &repo.root,
        &["worktree", "add", plain.to_str().unwrap(), "-b", "plain"],
    );

    let (seeded, plain) = (rebuilt(&seeded), rebuilt(&plain));
    assert!(
        !seeded.iter().any(|unit| unit.contains("third")),
        "the dependency is reused: {seeded:?}"
    );
    assert!(
        seeded.iter().any(|unit| unit.contains("app")),
        "the workspace moved with the checkout and is rebuilt: {seeded:?}"
    );
    assert!(
        plain.iter().any(|unit| unit.contains("third")),
        "without seeding it is built again: {plain:?}"
    );
}

#[test]
fn dry_run_adds_the_worktree_and_copies_nothing() {
    let repo = Repo::new();
    let worktree = repo.worktree("dry");

    repo.add(&["--dry-run"], &[worktree.to_str().unwrap()])
        .success()
        .stdout(contains("would copy"));

    assert!(worktree.join("ws/Cargo.toml").is_file(), "git still ran");
    assert!(!worktree.join("ws/target").exists());
}

#[test]
fn a_failing_git_call_seeds_nothing() {
    let repo = Repo::new();
    let worktree = repo.worktree("twice");
    repo.add(&[], &[worktree.to_str().unwrap()]).success();

    // The same path again: git refuses, and the tool says so.
    repo.add(&[], &[worktree.to_str().unwrap()])
        .code(EXIT_FAILURE)
        .stderr(contains("git worktree add"))
        .stderr(contains("already exists"));
}

#[test]
fn nothing_to_seed_from_is_said_and_is_not_a_failure() {
    let repo = Repo::new();
    fs::remove_dir_all(repo.target()).unwrap();
    let worktree = repo.worktree("cold");

    repo.add(&[], &[worktree.to_str().unwrap()])
        .success()
        .stdout(contains("nothing to seed from"));

    assert!(!worktree.join("ws/target").exists());
}
