# cargo-tare

A cargo subcommand that shrinks live `target/` directories without slowing builds: transparent
APFS compression, copy-on-write dedupe across targets, clone-seeding of new worktrees, and opt-in
removal of orphaned or idle targets — planned together so the approaches reinforce each other.
Design in `DESIGN.md`, measurements in `docs/research.md`.

| # | Status | Priority | Complexity | Readiness | Agent |
| --- | --- | --- | --- | --- | --- |
| T18 | todo | P3 | 3 | 0% | |

Blockers: none. Everything is free to start: the engine (`src/engine.rs`), the inode model
(`src/model.rs`), the inventory with families (`src/inventory.rs`), the hash index
(`src/index.rs`) and the test harness with the freshness oracle (`tests/common/mod.rs`) exist.
T13–T18 were added from the competitor review in `docs/research.md`; each card says which tool
does the same thing today.

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
