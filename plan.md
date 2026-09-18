# cargo-tare

A cargo subcommand that shrinks live `target/` directories without slowing builds: transparent
APFS compression, copy-on-write dedupe across targets, clone-seeding of new worktrees, and opt-in
removal of orphaned or idle targets — planned together so the approaches reinforce each other.
Design in `DESIGN.md`, measurements in `docs/research.md`.

| # | Status | Priority | Complexity | Readiness | Agent |
| --- | --- | --- | --- | --- | --- |
| T15 | todo | P2 | 3 | 0% | |
| T18 | todo | P3 | 3 | 0% | |

Blockers: none. Everything is free to start: the engine (`src/engine.rs`), the inode model
(`src/model.rs`), the inventory with families (`src/inventory.rs`), the hash index
(`src/index.rs`) and the test harness with the freshness oracle (`tests/common/mod.rs`) exist.
T13–T18 were added from the competitor review in `docs/research.md`; each card says which tool
does the same thing today.

### T15. Lossy pass: `orphan-toolchain` report

After a toolchain upgrade the artifacts built by the old rustc stay in the same profile dir
forever; cargo never revisits them. `cargo-sweep` covers this with `--installed` /
`--toolchains`, and does it by parsing hashed file names, which this project will not do.

The layout-independent source of truth is cargo's own fingerprint data: every
`.fingerprint/<unit>/*.json` records the rustc it was built with. First step is a report, not a
deletion: group the fingerprints by rustc, attribute bytes to each group, and show in `status`
how much of a target belongs to a rustc that is no longer the current one. Deleting those units
needs the unit-to-file mapping that only build-dir layout v2 gives (roadmap `R1`), so this task
ends at the number and an `advise` line. Done: the report is right on a fixture built with two
toolchains, and says nothing when there is only one.

### T18. Dedupe across families

Dedupe compares targets inside a family (a repository and its worktrees), because that is where
the duplicates are. `fclones` and `jdupes` compare everything, and unrelated projects do share
bytes: the same version of the same crate built with the same features is byte-identical, and the
hash index already holds the hashes needed to find that out. What is missing is not the
comparison but the locking: cloning across families means holding two families' locks at once,
and the benchmark run showed a family's own pass takes ~80 s, so a wider lock is a real cost.

Do it as an opt-in (`--across-families`), keep the sorted lock order that makes deadlock
impossible, and measure the extra yield on the benchmark workspace before making it a default.
Done: `docs/bench.md` gains the number, and a test proves two unrelated fixtures share a file
without either build going stale.
