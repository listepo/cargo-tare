# Done

### T9.1. Lossy pass: `orphans`

P0: on the measured machine 113.9 GB of ~158 GB sat in 30 worktree checkouts that git no longer
registered (sources and a `.git` file still present, last build a week old). Whole-dir removal
only, and only of `target/` — sources in such checkouts may hold uncommitted work that git can no
longer report. An orphan is a target whose project's `.git` file points at a missing worktree
record (the inventory already reports it). Off by default, always listed in `--dry-run` output
with the reason. Done: never touches a dir whose lock is held; a test covers a removed worktree.

Execution plan: the inventory already sets `Target.orphaned`; make the check reusable as
`inventory::is_orphaned(target)` so the pass can repeat it after locking, the way `evict`
re-reads the last build. `src/orphans.rs`: the `Orphans` pass (lossy) takes the orphaned targets
the inventory found, with their allocated bytes, and plans one `Action::RemoveTarget` per target
that is still orphaned. Engine: `RemoveTarget` removes the whole target dir — `doc/`, `package/`,
`CACHEDIR.TAG` and all — but only when at least one of the dirs we hold a lock for is inside it
and none of the dirs reported busy is, so a target with a running build is never touched. That
guard is what T17 will reuse. CLI: `--lossy orphans`, no threshold, reason in the report on a
dry run too. Tests (`tests/orphans.rs`, real `git worktree` on throwaway dirs): a removed
worktree record makes the target go whole, a live worktree's target stays, a busy profile keeps
the whole target, a record restored between inventory and lock keeps it, the pass does nothing
unless named, and a dry run lists without removing. Verify: `just check`.

Outcome: `src/orphans.rs` — the lossy `Orphans` pass takes the orphaned targets the inventory
found (with their allocated bytes) and plans one `Action::RemoveTarget` per target that still
holds a locked profile dir and that `inventory::is_orphaned` (new, the old private check made
reusable) still calls an orphan. `src/engine.rs` gained `Action::RemoveTarget { dir, reason,
bytes }` and a shared `remove()` helper: the target goes whole (`doc/`, `package/`,
`CACHEDIR.TAG` and all) only when a lock we hold is inside it and no busy dir is under it,
otherwise `Skip::Unlocked`; removed dirs leave the locked set, so later passes do not scan them.
CLI: `--lossy orphans` / `--pass orphans`, no threshold, reasons printed on a dry run too.

`tests/orphans.rs`, 7 tests on real `git worktree` checkouts in temp dirs: the whole target of
an orphan goes and the checkout (including uncommitted work) stays; a live worktree is
untouched; a running build keeps the whole target; a record restored between the inventory and
the lock keeps it; nothing happens unless the pass is named, and a dry run only lists; an A/B
pair of identical trees where the run without the pass keeps exactly the bytes the run with it
frees; and the CLI end to end. `just check` green (fmt, clippy `-D warnings`, 37 tests).

Known limits, as documented in `DESIGN.md`: a target with one busy profile is kept entirely,
even when its other profiles are free; removal is not atomic, so an interrupted run leaves a
half-removed target for the next run to finish; a checkout whose common dir merely sits on an
unmounted volume reads as an orphan, because the `.git` file points at a path that does not
exist and nothing else distinguishes the two. Measured only on fixtures — the 113.9 GB of real
orphans that motivated the P0 have not been touched.

### T11. Benchmarks: size and build time

On a real mid-size workspace, `hyperfine`, clean and incremental builds: baseline vs compress vs
dedupe vs fused vs seeded worktree vs shared `build-dir` vs sccache. Done: `docs/bench.md` with
numbers and the defaults (`min-age`, `min-size`) justified by them. Needs free disk space.

Execution plan: benchmark a **copy** of `apps/ketch` (creator's choice) — `git archive`/`rsync`
of the sources into a temp dir, a fresh `target/` built there; the tool never sees a real target.
Enabling flags first, because the defaults make a benchmark impossible: everything just built is
younger than `min-age`, so `run` would skip it, and the passes cannot be told apart. Add to
`run`: `--pass <NAME>...` (which lossless passes to run, default all), `--min-age <SECS>` and
`--min-size <BYTES>`. T10 plans the same switches in the config, so this is that surface, not a
throwaway. `scripts/bench.sh` then drives `hyperfine` over the variants: baseline, compress,
dedupe, both fused, sccache; per variant a clean build, an incremental build after touching one
workspace file, and `du -sk` of the target before and after the tool; plus the tool's own runtime
and a freshness check (`cargo build --message-format=json` must report no unit rebuilt). The
`min-age` / `min-size` sweep reuses the same script. Not measurable yet and to be written down as
such: seeded worktree (T8), shared `build-dir` (nightly-only `-Z build-dir`; this machine is on
stable 1.98). Results and the raw hyperfine JSON go to `docs/bench.md`, which also justifies the
defaults. Verify: `just check`, and the bench script run end to end on the copy.

Outcome: `scripts/bench.sh` (also `just bench <workspace>`) measures a **copy** of a workspace —
a shallow clone plus one git worktree of that clone, so the two targets form a family; the real
target dir is never touched. Per stage it records the tool's wall clock, `du` and free space
before and after, how many units cargo rebuilds, and hyperfine means for incremental builds.
Enabling flags, planned for the config in T10 and added here because the defaults make a
benchmark impossible: `--pass <NAME>` (repeatable), `--min-age <SECS>`, `--min-size <BYTES>`;
`main.rs` now carries a `RunArgs` struct, and `compress::NAME` / `dedupe::NAME` exist beside
`evict::NAME`. Tests: `tests/cli.rs` covers an unknown `--pass` name, a run limited to one pass,
and both floors; the fake-target helper moved to `tests/common/mod.rs`. `just check` green.

Numbers (`docs/bench.md`, `apps/ketch`, 587 crates, two checkouts, two full runs): compress
takes the two targets from 3.63 GiB to 1.32 GiB in 81.7 s; dedupe frees another 407 MiB in
12.4 s — together a 74% cut of freshly built targets no age-based cleaner would touch. After
each pass cargo reported **0** units out of date. Incremental builds: baseline 3.70 s mean
(2.39–5.10), after compress 4.96 s (2.78–10.17, the first build after the rewrite is the
outlier), after both 3.53 s (2.77–5.14) — no measurable slowdown. sccache, for comparison: cold
clean build 83.5 s, warm 32.3 s, 304 MiB cache; complementary, not a substitute.

Learned and written down: `du` cannot see copy-on-write sharing, so dedupe is only visible in
free space; one pipeline run does not converge (46 actions left right after a full run, because
dedupe's clones are files compress never saw); `hyperfine` needs three warmups after a clean
build or the numbers are dominated by the machine settling. Not measured, with the reason in the
doc: seeded worktree (needs T8) and shared `build-dir` (nightly-only on this toolchain).

### T9.2. Lossy pass: `evict`

Profile dirs idle for N days, then least-recently-built first until the total is under
`max-total`. Whole profile dirs only. Off by default, always listed in `--dry-run` output with
the reason. Done: never touches a dir whose lock is held; tests cover the idle rule and a size cap.

Execution plan: the inventory reports every profile dir with its allocated bytes and last build
(`Target.profiles` becomes a list of `ProfileInfo`). `src/evict.rs`: `select` — a pure function
over all profiles under the roots: idle ones first, then least recently built until the total
fits `max-total`; each choice carries its reason. The `Evict` pass (lossy) plans
`Action::RemoveProfile` only for selected dirs whose lock the engine holds and whose last build
still equals the inventory's — a build that slipped in between keeps its profile. Engine: the
dir must be one of the locked profile dirs; planned removals and reasons go to the report, dry
run included; a removed dir leaves the set that later passes scan. CLI until T10 brings the
config: `--lossy evict` with `--evict-idle-days <N>` and / or `--evict-max-total-gib <N>`, and
an error when neither is given. Tests: unit tests of `select`; `tests/evict.rs` on fake profile
dirs with aged mtimes — idle rule, size cap order, dry run, busy dir untouched, profile built
after the inventory kept, not run unless named; one CLI run end to end. Verify: `just check`.

Outcome: `src/evict.rs` — `select` (pure, global, reasons attached) and the lossy `Evict` pass;
the inventory reports `ProfileInfo` per profile dir; the engine got `Action::RemoveProfile`
(only an exactly locked profile dir, removed whole like `cargo clean`, dropped from later
passes) and `PassReport::removals`, filled on dry runs too. CLI: `--lossy evict` with
`--evict-idle-days` and / or `--evict-max-total-gib`; either without the other is an error before
anything is touched. Tests: 4 unit tests of `select`; `tests/evict.rs` — idle rule, cap order,
not run unless named, dry run lists only, profile with a held lock untouched, profile built after
the inventory kept, CLI end to end. fmt, clippy `-D warnings` and the suite green three runs in
a row. Documented in `DESIGN.md` "Evict pass" and `README.md`. Known limits: a busy profile is
skipped, so a run may end above the cap; the cap uses inventory sizes taken before compress and
dedupe; a profile dir vanishing between inventory and lock fails the run; thresholds move to the
config in T10; tested on fake profile dirs only, never on a real target.

### T6. Compress pass

Transparent APFS compression through the `applesauce` library (chosen in T2); skip compressed,
small and hot (`min-age`) inodes; whole hardlink groups only. Find out why T2 saw a single 119 MB
file left uncompressed — real targets hold 130 MB rlibs. Done: oracle green, fixture target
shrinks, a large file compresses or its limit is documented, second run is a no-op. Runs before
dedupe and compresses only canonicals and unique files (dedupe already prefers a compressed
canonical); compressing in place must keep or refresh the inode's entry in the hash index.

Execution plan: the library skips files with more than one link and replaces the inode, so the
engine never lets it near a live file. New `Action::Compress(Inode)`: the engine checks the group
like for a replacement, clones its first path into a sibling temp, hands a batch of temps to
`Pass::compress`, and swaps in — through `rename`, the other paths through `hard_link` — only
temps that came back with the compressed flag, after checking the group's stamps again; mtime and
mode are restored by the engine. `Pass::rewritten(old, new)` goes to every pass so dedupe moves
its index entry to the new inode. `src/compress.rs`: `Compress` plans unflagged inodes of at
least 8 KB, older than `min-age`, not marked shared in the index (compressing a clone un-shares
it); backend `applesauce` (LZFSE, level 5, ratio 0.95, verify on), its skip reasons and errors
are kept for the report. The hash index becomes a `RefCell` owned by the caller and borrowed by
both passes. Tests in `tests/compress.rs`: oracle green and target smaller on the fixture, a
hardlink group stays one inode with mtime and mode kept, second run plans nothing, small / hot /
shared / flagged files left alone, a 130 MiB file, dedupe after compress hashes nothing twice and
keeps clones compressed. Verify: `just check`.

Outcome: built as planned with `applesauce` 0.8.8 (added to the shared `rust.md` inventory).
7 tests in `tests/compress.rs`: the fixture profile shrinks by exactly the reported
`freed_bytes`, the oracle stays green and a second run applies nothing; a hardlink group stays
one inode with mtime and mode kept; small, hot, shared and flagged files are not planned; an
incompressible file keeps its inode, reads `NotCompressed` and the backend's reason is reported;
a 130 MiB file compresses; dedupe after compress leaves compressed clones and an idle second
run; an index entry follows its file to the new inode. 30 rounds of the file in a row and the
whole suite three times green, `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
clean. The 119 MB file of T2 was not reproduced: size is not the limit, the backend skipped that
blob for a reason the spike did not capture; the pass now captures such reasons. Differs from the
card: every eligible duplicate is compressed before dedupe folds it (CPU, not space) instead of
canonicals only — that needs content hashes before compression and is left to the benchmarks.
Found on the way: the fork race assumed in T5 is real (3 of 20 rounds failed until
`run_unbusy` summed its attempts); it needs test threads that spawn processes and cannot happen
in the tool. Not verified: decompression cost at link time (T11), files of 4 GiB and more,
behaviour on a real target dir.

### T3. Test harness and freshness oracle

A small fixture workspace (third-party deps, a proc-macro, a build script, a test binary) built in
a temp dir, plus helpers: allocated-bytes measurement (`st_blocks`) and the oracle from
`DESIGN.md` — after a pass, `cargo build --message-format=json` reports every unit fresh and
`cargo test` passes. Done: the oracle fails when a deliberately broken pass changes an mtime.

Execution plan: `tests/common/mod.rs` — `Fixture` (a workspace with a bin + lib package that has
a build script writing into `OUT_DIR`, unit and integration tests, a proc-macro member, and a
path dependency outside the workspace standing in for a third-party crate: hermetic, builds
`--offline`; registry crates differ only in how their sources are fingerprinted, which the tool
never touches), `stale_units` / `assert_fresh` (JSON messages of `cargo build` and
`cargo test --no-run` parsed with `serde_json`, then `cargo test`), `allocated_bytes`
(`model::scan`), `run_unbusy` (the retry both test files duplicate today). `tests/engine.rs` and
`tests/dedupe.rs` move to the fixture and the oracle; `tests/harness.rs` proves the oracle: green
on an untouched build, reports stale units after mtimes of `.rlib` files are bumped. CLI tests
per `rust.md`: `trycmd` for full output (`tests/cmd/*.trycmd`), `assert_cmd` + `predicates` for
exit codes. Crates (dev): `trycmd`, `assert_cmd`, `predicates`. Verify: `just check`.

Outcome: built as planned; dev crates `trycmd` 1.2.1, `assert_cmd` 2.2.2, `predicates` 3.1.4.
`tests/harness.rs`: the oracle is green on an untouched build and reports stale units once the
mtimes of the `.rlib` files are bumped, then is green again after cargo's rebuild. The real-cargo
tests of `tests/engine.rs` and `tests/dedupe.rs` now use the fixture and `assert_fresh`, and the
duplicated busy-retry and `ino` helpers are gone. `tests/cli.rs`: three `trycmd` cases (version,
`status --help`, `run` without a root exits 2) and four exit-code tests. The whole suite green
three runs in a row, `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean.
Differs from the card: the third-party dependency is a path crate outside the workspace, not a
registry crate, so the suite needs no network. The "broken pass" is a direct mtime bump in the
test: the engine has no action that skips restoring the mtime, so such a pass cannot be written
against it. Not covered: `[patch]`, registry and git dependencies, custom profiles, a
`--target <triple>` layout.

### T4. Inventory, discovery and `status`

Walk configured roots, detect cargo target / build dirs by cargo's own `CACHEDIR.TAG` text, find
profile dirs, build the inode model (hardlink groups, size, mtime, compressed flag), group targets
into families by git common dir. `cargo tare status` prints the inventory and estimated savings,
with `--json`. Read-only. Done: matches `du` within 1% on the fixture and ignores non-cargo caches.
Also: `run` without arguments takes every discovered target, one family per engine run, so the
dedupe pass (which works across whatever profile dirs one run gets) searches inside a family.

Execution plan: `src/inventory.rs` — `discover(roots)` (walk, stop at every dir cargo tagged),
`git_link` (nearest `.git`: a dir is the common dir; a file is followed through `gitdir:` and
`commondir`; a missing record marks the target orphaned and the family is taken from the record's
path), per-target totals from `model::scan` of the whole target dir (inodes, paths, logical,
allocated, already compressed, compressible, last built), per-family upper bound for dedupe (bytes
of files whose size also occurs in a sibling target). `cargo tare status [--json] [ROOT]…` and
`cargo tare run` with roots instead of target dirs: one engine run per family. Crates: `serde`,
`serde_json`. Tests in `tests/inventory.rs`: `du` within 1% (hardlinks included), foreign
`CACHEDIR.TAG` ignored, nested targets not entered, main repo + worktree form one family, a removed
worktree record reads as orphaned, JSON parses. Verify: `just check`.

Outcome: built as planned; `serde` 1.0.229 and `serde_json` 1.0.151 added. 4 tests in
`tests/inventory.rs` (size within 1% of `du -sk` with a hardlink group and files outside profile
dirs; a Gradle-tagged dir ignored and a nested tagged dir not reported; repository + `git worktree`
form one family, an unrelated project has none, a deleted worktree record reads as orphaned with
the family kept, the dedupe estimate is non-zero only inside the family; `status --json` parses).
The whole suite green three runs in a row, `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` clean. Differs from the card: `run` requires at least
one `<ROOT>` instead of defaulting to every discovered target — there are no configured roots
before T10, and a mutating command should not default to the current dir. Not verified: the tool
was never pointed at a real target dir, so `status` has not been compared with the measurements
in `docs/research.md`; a git submodule has no `commondir` file, so it reads as its own family —
untested.

### T7. Hash index and fused dedupe pass

Persistent hash cache keyed by `(device, inode, size, mtime)`, size-bucket prefilter, family-first
candidate search, `clonefile` replacement, fusion with compress (compress the canonical inode,
clone it over the group; per-group fallback if clones of compressed files do not share). Done:
oracle green on two sibling fixture targets, re-run after a small rebuild hashes only new inodes.

Taken ahead of T3 / T4 / T6 at the creator's request. Scope here: the index and the dedupe pass
over all profile dirs given to one run. Left to their own tasks: family-first narrowing (T4),
compressing the canonical before cloning (T6 — this pass already prefers a compressed canonical,
so T6 only has to compress canonicals and unique files first), the oracle on a fixture with real
dependencies (T3; here two sibling builds of a dependency-free fixture).

Execution plan: `src/index.rs` — `(dev, ino) → (size, mtime, sha256, shared)` in one flat binary
file, loaded at start, saved through temp + `rename`; `shared` marks inodes this tool cloned or
cloned from, so a second run does not clone them again. `src/dedupe.rs` — filter (`min-size`,
`min-age`, replaceable), bucket by `(dev, size)`, hash only buckets with two or more inodes
(`sha2`, `rayon`, index first), group by hash, canonical = shared, then compressed, then oldest;
members = unshared inodes. Engine: `Pass::replaced` hook so the pass can record the new inode.
CLI: `run --index <FILE>`, default `~/.cache/cargo-tare/hashes-v1.bin`. Tests in
`tests/dedupe.rs`: duplicates across two profiles, second run is a no-op and hashes nothing, a
rewritten file is rehashed, hot and small files are left alone, index survives a corrupt file,
two sibling cargo builds stay fresh. Verify: `just check`.

Outcome: built as planned with `sha2` 0.11 and `rayon` 1.12; 5 tests in `tests/dedupe.rs` and one
unit test for the canonical order, the whole suite green three runs in a row, `cargo fmt --check`
and `cargo clippy --all-targets -- -D warnings` clean. Two real builds of one source into two
target dirs did share at least one artifact, and both targets reported every unit fresh
afterwards. Documented in `DESIGN.md` ("Dedupe pass and hash index"), `README.md`,
`toolchain.md`. Not done here, moved into the cards of T4 (family-first via one engine run per
family), T6 (compress canonicals first, keep the index entry) and T8 (register seeded inodes as
shared). Not verified: the compressed-canonical preference is unit-tested on the ordering only,
no compressed file existed in the tests; real disk savings are not measured (clones are checked
by identity and content, block sharing needs `df` on a scratch volume — T11); never run on a real
target yet.

### T5. Engine: plan / apply pipeline and safety core

Pass trait producing actions against the shared inventory; ordered pipeline; `--dry-run`; cargo
lock acquisition (`try_lock`, sorted order, skip busy dirs); atomic group replacement; pre-apply
re-check of size and mtime; cleanup of leftover temp files; per-pass byte report. Done: every
safety invariant in `DESIGN.md` has a test, including a build running concurrently.

Taken ahead of T3 / T4 at the creator's request, so it carries the two pieces it cannot work
without: the per-profile inode model (`src/model.rs`; T4 keeps discovery, families, `status`) and
a dependency-free cargo fixture for the concurrent-build test (T3 keeps the full fixture and the
reusable oracle).

Execution plan: `src/lib.rs` + `src/model.rs` (scan one profile dir into inodes with all their
paths; no symlink following, one device) + `src/engine.rs` (`Pass` trait, `Action::Replace`,
`ProfileLock` over `.cargo-lock` with `File::try_lock`, sorted lock order, stale temp cleanup,
re-check, clone → restore mtime / mode → `rename`, hardlink the rest of the group, per-pass
report). `cargo tare run [--dry-run] [--lossy <pass>] <target-dir>…` wired with zero passes.
Crates: `walkdir`, `anyhow`, dev `tempfile`; clone through `std::fs::copy` (uses `fclonefileat`
on APFS). Tests in `tests/engine.rs`, one per invariant, plus a real `cargo build` holding the
lock and a freshness check afterwards. Verify: `just check`.

Outcome: 12 tests in `tests/engine.rs`, green three runs in a row, with `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` clean. Covered: group replacement keeping one inode,
mtime and mode; busy profile untouched; stale temps; `--dry-run`; member changed after the scan;
links outside the profile; flagged inode; paths outside locked dirs; lossy gating; no symlink
following; foreign `CACHEDIR.TAG`; and a real `cargo build` that reads as busy while it runs and
reports every unit fresh after its final binary's hardlink group was replaced. Documented in
`DESIGN.md` ("Engine"), `README.md`, `toolchain.md`. Not covered: the device-boundary rule is
enforced (`same_file_system`, `CrossDevice`) but has no test, it needs a second volume; whether
the clone really shares blocks is measured in T7 / T11, the engine test only checks identity and
content. Gotcha for later tests: one test that runs the engine twice on the same dir failed once
with the second run seeing nothing to do. Most likely cause (not proven): a lock fd open while
another test thread spawns a process stays held by that child until it execs, so the dir briefly
reads as busy. The `run` helper in `tests/engine.rs` retries while busy; green since.

### T1. Scaffold the project

Cargo binary crate `cargo-tare` (latest stable Rust pinned via mise and `rust-toolchain.toml`),
git repository, lint / format / test commands, CI-free for now. Done: `cargo tare --version`
runs, `toolchain.md` lists what is actually used.

Execution plan: `Cargo.toml` (edition 2024, `publish = false`, `clap` with `derive` added via
`cargo add` to get the latest version), `rust-toolchain.toml` + `mise.toml` pinned to 1.98,
`src/main.rs` with the `cargo tare` subcommand wrapper and nothing else, one integration test for
`--version` on std only, `Justfile` with `check` (fmt, clippy `-D warnings`, test), `.gitignore`,
`git init` without committing. Verify: `cargo run -- tare --version` and `just check`.

Outcome: builds on rustc 1.98.1 with `clap` 4.6.7; `cargo run -- tare --version` prints
`cargo-tare 0.1.0`; `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test` (1 integration test) are green. The three `just check` steps were run one by one, not
through `just`. Repository initialised on `main`, nothing committed. Gotcha: a fresh `mise.toml`
is untrusted, so every `cargo` call fails until `mise trust` is run once in the project.

### T2. Spike: APFS primitives and backend choice

Answer on a scratch fixture, with a throwaway binary or script, before any real code:
(a) does `clonefile` + restored mtime keep cargo units fresh; (b) how to replace an inode for a
whole hardlink group atomically; (c) compression backend — `applesauce` as a library vs own
decmpfs writer: hardlink handling, real LZFSE ratio including small files, CPU cost; (d) do clones
of a compressed file share blocks; (e) does a recursive clone of a target make registry deps fresh
in another worktree; (f) hashing: `blake3` vs `sha2` throughput. Done: answers and numbers
recorded in `DESIGN.md`, dependency list sent to the creator for approval.

Execution plan: one bash script (`docs/spike/t2-spike.sh`) that runs entirely inside a throwaway
APFS sparse image, so `df` deltas are exact and no real target is touched. Fixture: a two-member
workspace (serde derive, serde_json, memchr, libc) built offline, plus git worktrees of it.
Sections: E1 seed by recursive clone (with / without `-p`) vs cold build; E2 content match between
independently built worktrees, clone replacement with restored mtime, negative control without
mtime; E3 hardlink-group replacement; E4 compression via `ditto` (freshness, relink, clone sharing
of compressed files, write-to-clone); E5 hash throughput; E6 cargo lock interop via `flock`;
E7 `applesauce` CLI on a target with hardlinks (ratio, links, mtimes). Verify with the freshness
oracle (`cargo build --message-format=json`). Raw output goes to `docs/spike/`, conclusions to
`DESIGN.md`. `blake3` and `applesauce` were approved by the creator.

Outcome: (a), (b), (d), (e) confirmed; (c) `applesauce` chosen (−65% on the fixture, hardlinks,
mtimes and modes intact); (f) `sha2` first (hardware SHA-256 1484 MB/s), `blake3` approved but not
needed yet. The `ditto` section compressed nothing and was replaced by `t2-spike-e8.sh`. Left
open: `applesauce` did not compress a single 119 MB blob (goes to T6), decompression cost at link
time (goes to T11). Results table: `DESIGN.md`, "T2 spike results". Raw output: `docs/spike/`.
