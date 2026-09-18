//! Which rustc built what, read from cargo's own fingerprints.
//!
//! After a toolchain upgrade the artifacts the old rustc produced stay in the profile dir
//! forever: cargo compiles the units again under new hashes and never revisits the old ones.
//! The layout-independent way to see that is `<profile>/.fingerprint/<unit>/*.json`, where
//! cargo records the compiler it used as a hash of its version. A unit is a directory and the
//! compiler is a number cargo wrote — no file name is parsed here, which is the whole reason
//! this is not done the way `cargo-sweep --installed` does it.
//!
//! This is a report. Removing those units needs the unit-to-file map that only cargo's newer
//! build-dir layout gives (roadmap `R1`); until then the cure is `cargo clean`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Cargo's dir of fingerprints, one subdir per unit, inside every profile dir.
pub const DIR: &str = ".fingerprint";

/// The units one rustc built in a profile dir.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Built {
    /// Cargo's own hash of the compiler version. Opaque: it identifies a rustc, it does not
    /// name one, and nothing here tries to turn it back into a version.
    pub rustc: u64,
    pub units: usize,
    /// Newest fingerprint of the group, as unix seconds.
    pub newest_unix: Option<u64>,
}

/// Only the one field that matters; cargo writes a dozen more.
#[derive(Deserialize)]
struct Fingerprint {
    rustc: u64,
}

/// Units per rustc over some profile dirs, the most recently used compiler first. Empty when
/// they hold no fingerprints — an unbuilt profile, or a cargo old enough to write none. A
/// target's profiles are counted together: `debug` and `release` are compiled by the same rustc,
/// and what the report is about is the target.
pub fn scan(profile_dirs: &[PathBuf]) -> Vec<Built> {
    // Ordered on the timestamp itself, not on the seconds the report prints: two builds of the
    // same second are still two builds.
    let mut groups: HashMap<u64, (usize, Option<SystemTime>)> = HashMap::new();
    let units = profile_dirs
        .iter()
        .flat_map(|dir| fs::read_dir(dir.join(DIR)).into_iter().flatten());
    for unit in units.flatten() {
        let files = fs::read_dir(unit.path()).into_iter().flatten();
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            // A fingerprint we cannot read or parse is a unit we cannot attribute; the count
            // says "of the units we could read", which is what the report claims.
            let Some(fingerprint) = fs::read(&path)
                .ok()
                .and_then(|text| serde_json::from_slice::<Fingerprint>(&text).ok())
            else {
                continue;
            };
            let built = file.metadata().ok().and_then(|meta| meta.modified().ok());
            let group = groups.entry(fingerprint.rustc).or_default();
            group.0 += 1;
            group.1 = group.1.max(built);
        }
    }
    let mut groups: Vec<(u64, usize, Option<SystemTime>)> = groups
        .into_iter()
        .map(|(rustc, (units, newest))| (rustc, units, newest))
        .collect();
    // Newest first, so the head is the compiler cargo is using now. A tie is broken by the
    // rustc hash, only so that the order is stable.
    groups.sort_by_key(|&(rustc, _, newest)| (std::cmp::Reverse(newest), rustc));
    groups
        .into_iter()
        .map(|(rustc, units, newest)| Built {
            rustc,
            units,
            newest_unix: newest.and_then(unix),
        })
        .collect()
}

/// Units built by a compiler that is no longer the one cargo uses here, of the units read.
/// Zero when only one rustc appears, which is the ordinary case.
pub fn stale(builds: &[Built]) -> usize {
    builds.iter().skip(1).map(|built| built.units).sum()
}

fn unix(time: SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|since_epoch| since_epoch.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::time::Duration;

    /// One unit's fingerprint, built `age_secs` ago.
    fn fingerprint(profile: &Path, unit: &str, file: &str, rustc: u64, age_secs: u64) {
        let dir = profile.join(DIR).join(unit);
        fs::create_dir_all(&dir).unwrap();
        let json = dir.join(format!("{file}.json"));
        fs::write(
            &json,
            format!("{{\"rustc\":{rustc},\"features\":\"[]\",\"deps\":[]}}"),
        )
        .unwrap();
        let built = SystemTime::now() - Duration::from_secs(age_secs);
        fs::File::options()
            .write(true)
            .open(&json)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(built))
            .unwrap();
        // Cargo writes these beside the JSON; they must not be counted as units.
        fs::write(dir.join(file), "").unwrap();
        fs::write(dir.join("invoked.timestamp"), "").unwrap();
    }

    #[test]
    fn one_compiler_is_no_finding() {
        let tmp = tempfile::TempDir::new().unwrap();
        fingerprint(tmp.path(), "serde-aaaa", "lib-serde", 7, 60);
        fingerprint(tmp.path(), "libc-bbbb", "lib-libc", 7, 30);

        let builds = scan(&[tmp.path().to_path_buf()]);

        assert_eq!(builds.len(), 1);
        assert_eq!(builds[0].units, 2);
        assert_eq!(stale(&builds), 0);
    }

    #[test]
    fn units_of_the_older_compiler_are_the_stale_ones() {
        let tmp = tempfile::TempDir::new().unwrap();
        fingerprint(tmp.path(), "old-aaaa", "lib-old", 7, 86_400);
        fingerprint(tmp.path(), "old-bbbb", "lib-older", 7, 86_000);
        // The newest fingerprint, so its compiler is the one cargo is using now.
        fingerprint(tmp.path(), "new-cccc", "lib-new", 9, 60);

        let builds = scan(&[tmp.path().to_path_buf()]);

        assert_eq!(builds.len(), 2);
        assert_eq!(builds[0].rustc, 9, "newest first: {builds:?}");
        assert_eq!(stale(&builds), 2);
    }

    #[test]
    fn a_profile_without_fingerprints_says_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();

        assert!(scan(&[tmp.path().to_path_buf()]).is_empty());
    }
}
