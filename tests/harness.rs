//! The oracle has to be able to fail, or a green run proves nothing.

use std::fs::{self, File};
use std::time::SystemTime;

mod common;
use common::{Fixture, allocated_bytes};

#[test]
fn oracle_is_green_on_an_untouched_build_and_sees_a_changed_mtime() {
    let fixture = Fixture::new();
    let target = fixture.target();
    fixture.build(&target);
    fixture.assert_fresh(&target);
    assert!(allocated_bytes(&target) > 0);

    // What a pass that forgets to restore mtimes does: every library is now newer than the
    // binaries linked from it.
    let deps = target.join("debug/deps");
    for entry in fs::read_dir(deps).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rlib") {
            let file = File::options().write(true).open(path).unwrap();
            file.set_modified(SystemTime::now()).unwrap();
        }
    }

    let stale = fixture.stale_units(&target);
    assert!(!stale.is_empty(), "the oracle missed changed mtimes");
    // Cargo rebuilt what it distrusted, so the next look is clean again.
    fixture.assert_fresh(&target);
}
