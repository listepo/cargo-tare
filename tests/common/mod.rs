//! Shared by the integration tests: a real cargo fixture, the freshness oracle, engine runs.
//! Everything lives in temp dirs; nothing here may point at a real project's target.

// Every test binary compiles this module and uses its own part of it.
#![allow(dead_code)]

use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use cargo_tare::engine::Report;
use cargo_tare::model::{self, CARGO_LOCK_FILE};
use tempfile::TempDir;

const CARGO_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n\
    # This file is a cache directory tag created by cargo.\n";
const SECS_PER_DAY: u64 = 24 * 60 * 60;
const KIB: usize = 1024;

pub const POLL: Duration = Duration::from_millis(20);
const LOCK_RACE_TIMEOUT: Duration = Duration::from_secs(2);
/// Read by the fixture's build script and not declared to cargo, so it never makes a unit stale.
pub const BUILD_SLEEP_ENV: &str = "TARE_FIXTURE_BUILD_SLEEP_SECS";
/// Name of the fixture's binary inside a profile dir.
pub const BIN: &str = "fx";

const FILES: &[(&str, &str)] = &[
    (
        "ws/Cargo.toml",
        "[workspace]\nmembers = [\"fx\", \"fx-macros\"]\nresolver = \"3\"\n",
    ),
    (
        "ws/fx/Cargo.toml",
        "[package]\nname = \"fx\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [dependencies]\nfx-dep = { path = \"../../vendor/fx-dep\" }\n\
         fx-macros = { path = \"../fx-macros\" }\n",
    ),
    (
        "ws/fx/build.rs",
        "use std::{env, fs, path::Path, thread, time::Duration};\n\
         fn main() {\n\
             println!(\"cargo::rerun-if-changed=build.rs\");\n\
             if let Ok(secs) = env::var(\"TARE_FIXTURE_BUILD_SLEEP_SECS\") {\n\
                 thread::sleep(Duration::from_secs(secs.parse().unwrap()));\n\
             }\n\
             let out = Path::new(&env::var(\"OUT_DIR\").unwrap()).join(\"generated.rs\");\n\
             fs::write(out, \"pub const GENERATED: u32 = 1;\\n\").unwrap();\n\
         }\n",
    ),
    (
        "ws/fx/src/lib.rs",
        "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n\
         pub fn total() -> u32 { fx_dep::double(fx_macros::answer!()) + GENERATED }\n\
         #[cfg(test)]\nmod tests {\n    #[test]\n    fn total() { assert_eq!(super::total(), 85); }\n}\n",
    ),
    (
        "ws/fx/src/main.rs",
        "fn main() { assert_eq!(fx::total(), 85); }\n",
    ),
    (
        "ws/fx/tests/smoke.rs",
        "#[test]\nfn total() { assert_eq!(fx::total(), 85); }\n",
    ),
    (
        "ws/fx-macros/Cargo.toml",
        "[package]\nname = \"fx-macros\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [lib]\nproc-macro = true\n",
    ),
    (
        "ws/fx-macros/src/lib.rs",
        "use proc_macro::TokenStream;\n\
         #[proc_macro]\npub fn answer(_: TokenStream) -> TokenStream { \"42u32\".parse().unwrap() }\n",
    ),
    // Outside the workspace, the way a third-party crate is: a dependency, not a member.
    (
        "vendor/fx-dep/Cargo.toml",
        "[package]\nname = \"fx-dep\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    ),
    (
        "vendor/fx-dep/src/lib.rs",
        "pub fn double(x: u32) -> u32 { x * 2 }\n",
    ),
];

/// A cargo workspace in a temp dir: a bin + lib package with a build script, unit and
/// integration tests, a proc-macro member and a dependency from outside the workspace.
pub struct Fixture {
    _tmp: TempDir,
    pub root: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for (path, content) in FILES {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        Self { _tmp: tmp, root }
    }

    /// The default target dir, `<workspace>/target`.
    pub fn target(&self) -> PathBuf {
        self.root.join("ws/target")
    }

    pub fn cargo(&self, target: &Path, args: &[&str]) -> Command {
        cargo_at(&self.root.join("ws"), target, args)
    }

    /// Builds everything the oracle looks at: `cargo build` and the test binaries.
    pub fn build(&self, target: &Path) {
        for args in ORACLE_BUILDS {
            assert!(self.cargo(target, args).status().unwrap().success());
        }
    }

    /// Units cargo does not consider fresh. Asking rebuilds them, so a second call is clean.
    pub fn stale_units(&self, target: &Path) -> Vec<String> {
        stale_units_at(&self.root.join("ws"), target)
    }

    /// The oracle of `DESIGN.md`: nothing is rebuilt, and what is there still works.
    pub fn assert_fresh(&self, target: &Path) {
        assert_eq!(self.stale_units(target), [] as [String; 0]);
        assert!(self.cargo(target, &["test"]).status().unwrap().success());
        let bin = target.join("debug").join(BIN);
        assert!(Command::new(bin).status().unwrap().success());
    }
}

const ORACLE_BUILDS: [&[&str]; 2] = [&["build"], &["test", "--no-run"]];

/// A cargo run in `ws` writing into `target`, offline and speaking JSON.
pub fn cargo_at(ws: &Path, target: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(ws)
        .env("CARGO_TARGET_DIR", target)
        .args(args)
        .args(["--offline", "--message-format=json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// Units cargo does not consider fresh, in any checkout of the fixture. Asking rebuilds them,
/// so a second call is clean.
pub fn stale_units_at(ws: &Path, target: &Path) -> Vec<String> {
    let mut stale = Vec::new();
    for args in ORACLE_BUILDS {
        let out = cargo_at(ws, target, args)
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(out.status.success(), "cargo {args:?}");
        let mut artifacts = 0;
        for line in String::from_utf8(out.stdout).unwrap().lines() {
            let message: serde_json::Value = serde_json::from_str(line).unwrap();
            if message["reason"] != "compiler-artifact" {
                continue;
            }
            artifacts += 1;
            if message["fresh"] != true {
                stale.push(format!("{} {}", message["target"]["name"], args[0]));
            }
        }
        assert!(artifacts > 0, "cargo {args:?} reported no artifacts");
    }
    stale
}

/// The binary under test, with a config home of its own: a test must never read, or depend on,
/// the configuration of the machine it runs on.
pub fn tare(config_home: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::new(env!("CARGO_BIN_EXE_cargo-tare"));
    cmd.arg("tare").env("XDG_CONFIG_HOME", config_home);
    cmd
}

/// Bytes on disk under `dir`, every hardlinked inode once.
pub fn allocated_bytes(dir: &Path) -> u64 {
    let inodes = model::scan(dir).unwrap().inodes;
    inodes.iter().map(|inode| inode.allocated).sum()
}

pub fn ino(path: &Path) -> u64 {
    fs::metadata(path).unwrap().ino()
}

/// Repeats an engine run while it reads as busy. A lock fd that is open while another test
/// thread spawns a process lives on in that child until it execs, so a second run on the same
/// dir can find its own previous lock still held for a moment. Seen in about one of seven
/// rounds of `tests/compress.rs`. An attempt that found one dir busy has still worked on the
/// others, so the attempts are summed; `busy` is that of the last one.
pub fn run_unbusy(mut engine_run: impl FnMut() -> Report) -> Report {
    let started = Instant::now();
    let mut total = engine_run();
    while !total.busy.is_empty() && started.elapsed() <= LOCK_RACE_TIMEOUT {
        std::thread::sleep(POLL);
        let next = engine_run();
        total.busy = next.busy;
        total.temps_removed += next.temps_removed;
        for (sum, pass) in total.passes.iter_mut().zip(next.passes) {
            sum.planned += pass.planned;
            sum.planned_bytes += pass.planned_bytes;
            sum.applied += pass.applied;
            sum.freed_bytes += pass.freed_bytes;
            sum.skipped.extend(pass.skipped);
            sum.removals.extend(pass.removals);
        }
    }
    total
}

/// A cargo-tagged target with one profile dir of about `kib` KiB, last built `days` ago.
/// The age is the profile dir's own entries, which is what the inventory reads; the artifact
/// inside `deps/` stays as young as the call, which is what the pass age floors read.
pub fn fake_target(root: &Path, name: &str, kib: usize, days: u64) -> PathBuf {
    let target = root.join(name).join("target");
    let profile = target.join("debug");
    fs::create_dir_all(profile.join("deps")).unwrap();
    fs::write(target.join("CACHEDIR.TAG"), CARGO_TAG).unwrap();
    File::create(profile.join(CARGO_LOCK_FILE)).unwrap();
    fs::write(profile.join("deps/libx.rlib"), vec![1; kib * KIB]).unwrap();
    let built = SystemTime::now() - Duration::from_secs(days * SECS_PER_DAY);
    for entry in fs::read_dir(&profile).unwrap() {
        let entry = File::open(entry.unwrap().path()).unwrap();
        entry.set_modified(built).unwrap();
    }
    profile
}
