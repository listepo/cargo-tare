# Done

### T16. Lossy pass: `doc`

`target/doc` is fully regenerable by `cargo doc` and is usually tens to hundreds of MB.
`cargo clean --doc` does exactly this, and `kondo` / `cargo-clean-all` get it only by deleting
the whole target. A one-directory pass: `--lossy doc`, remove `<target>/doc` whole, reported
with its size like every other removal. Smallest task in the list and pure profit for anyone who
ever ran `cargo doc` once. Done: a test builds docs in the fixture, the pass removes them, the
build oracle stays green (docs are not part of the build graph).

Outcome: `src/doc.rs` plans one removal per target that has a `doc/` dir, gated by `--lossy doc`
and placed after `incremental` in the pipeline. Because `doc/` lies beside the profile dirs
rather than inside one, `Action::RemoveTarget` now names the guarding `target` and the `dir` to
remove separately; `orphans` and whole-target eviction pass the same path for both, and the
engine's guard is unchanged in what it allows them. The size comes from a new
`Target::doc_bytes`, summed in the scan the inventory already does.

Tests: `tests/doc.rs` on the real fixture with `cargo doc --no-deps` run into it — the A/B
`ab_only_the_named_run_removes_the_docs` (same built fixture twice, `--lossy doc` the only
difference; both sides then pass the freshness oracle, so docs really are outside the build
graph), a dry run that reports the dir with its measured size in JSON and removes nothing, and a
target with a held `.cargo-lock` that keeps its docs and exits 2. Docs: DESIGN.md "Doc pass" plus
the pipeline table, README.

### T12. `advise` and automation recipes

`cargo tare advise` reads the configs that decide how big a target grows and says what to change.
The checklist, from the comparison in `docs/research.md` — every line is something a competitor
either recommends or works around:

- profile keys that bloat a target: `debug` (`line-tables-only` instead of `true`), `debug = false`
  for `[profile.dev.package."*"]`, `split-debuginfo` (macOS leaves `.dSYM` trees otherwise),
  `strip` for release, `codegen-units`;
- `incremental`: what turning it off would save here, and that T13 is the cheaper answer;
- `[unstable]` keys that a stable toolchain silently ignores (this machine had some), including
  `-Zembed-metadata=no` and `-Ztrim-paths`, with the roadmap item that will use them;
- `cache.auto-clean-frequency` for the cargo home (stable since 1.88) — the cargo-cache /
  cargo-trim niche, which cargo now covers itself;
- families that could share a `build-dir` (stable since 1.91) and the lock contention that makes
  it a bad trade for parallel agents;
- `cargo-hakari` for workspaces that rebuild too often, and `sccache` for machines that rebuild
  from scratch a lot — neither shrinks a live target, and both compose with this tool;
- worktrees never seeded (needs T8).

Plus documented `just` and launchd examples for running the lossless passes after builds. Done:
advice reproduces the findings in `docs/research.md` on the measured machine, and each item
prints the file and key it is about.

Outcome: `cargo tare advise [--json] [ROOT]...` reads the manifests and cargo configs of the
projects the inventory finds, plus `$CARGO_HOME/config.toml`, and prints two lists. Findings come
from files through the pure `advise::review(file, kind, doc, nightly)`: `profile.<p>.debug` (all
three spellings of full debuginfo, and cargo's own default for `dev`), the `"*"` dependency
override, `profile.release.strip`, `split-debuginfo = "packed"`, `codegen-units = 1` in a dev
profile, `build.incremental`, an `[unstable]` table on a stable toolchain, and
`cache.auto-clean-frequency` in the cargo home. Notes come from the inventory: what `incremental/`
weighs under the roots (a new `Target::incremental_bytes`, summed in the existing scan), families
whose targets could share a `build-dir`, checkouts `seed` would fill, and orphaned worktrees. The
toolchain channel is only asked for when a config has an `[unstable]` table.

Checked against the machine the research was done on: `advise` over `packages/` reproduced
`docs/research.md`'s finding that `[unstable] no-embed-metadata` in `~/.cargo/config.toml` is
ignored on stable, and found the 6-target family and a checkout with no target dir.

Out of scope, deliberately: `cargo-hakari` and `sccache` help with rebuild time rather than with
the size of a live target, and no file says whether a workspace wants them; they stay in
`docs/research.md`. The `just` and launchd recipes are in README instead of the output.

Tests: 6 unit tests over literal TOML (including a manifest that has taken every piece of advice
and gets nothing) and `tests/advise.rs` — the A/B `ab_a_tuned_manifest_gets_no_profile_advice`
(same tree twice, the manifest the only difference), a finding-names-file-and-key test, a
snapshot test proving the command writes nothing, the JSON shape, and a broken TOML warning that
does not end the run. Docs: DESIGN.md "Advise command", README `advise` paragraph and a "Running
it automatically" section.

### T17. Whole-target eviction

`evict` today selects profile dirs. `cargo-clean-all` and `kondo` work at target granularity, so
they also take `doc/`, `package/`, `tmp/` and `CACHEDIR.TAG` — everything a target holds outside
its profile dirs. Add `--evict-whole-target`: when every profile dir of a target is selected,
remove the target dir itself rather than its profiles one by one. Needs the "remove a dir that
contains only locked profile dirs" guard that T9.1 introduces for orphans, so it is that task's
machinery applied to a second selector. Done: a test where a target with two profiles and a
`doc/` dir leaves nothing behind, and one where a busy profile keeps the whole target.

Outcome: `evict::whole_targets(targets, chosen)` (pure) returns every target whose profile dirs
the selection took whole, each carrying its profiles and its `du` bytes; `Evict::whole(...)` turns
the upgrade on, and `plan` emits one `Action::RemoveTarget` for such a target and drops the
per-profile `Remove` actions inside it. The re-check under the lock covers every profile of the
target, so a busy or freshly built profile keeps the target dir while the free profiles are still
evicted one by one. Wired as `--evict-whole-target` and `[evict] whole-target`.

Tests: `a_target_goes_whole_only_when_every_profile_of_it_is_chosen` (unit, over a partly idle
target and one with no profiles at all) and `tests/evict_whole.rs` — the A/B
`ab_only_the_whole_target_run_takes_what_is_outside_the_profiles` (same tree twice, the flag the
only difference: without it `doc/` and `CACHEDIR.TAG` survive, with it the target dir is gone and
the project around it stays), the busy-profile fallback, a fresh third profile keeping its target,
and the config-file route. Docs: DESIGN.md "Evict pass" and the CLI/config surface, README.

### T8. `seed`: clone-seed a new worktree's target

`cargo tare seed [--from <dir>] [<dir>]` with automatic source choice inside the family; excludes
`incremental/` and lock files. Done: in a fresh worktree of the fixture, the first build compiles
workspace members only and the seeded target adds ~0 allocated bytes. Register the seeded inodes
in the hash index as shared (`src/index.rs`), otherwise the first dedupe run clones them again.

Execution plan: `src/seed.rs` — `choose(dest)` picks the source inside the family: the git common
dir of the checkout (`inventory::family`), its registered checkouts (`inventory::checkouts`,
reading `<common>/worktrees/*/gitdir`), and of those the target with the newest build. `--from`
names one instead. `seed` walks the source target with `walkdir`, recreates dirs and symlinks and
`fs::copy`s every file — `clonefile` on APFS, so the copy shares blocks and costs no space —
skipping `incremental/`, `.cargo-lock` and leftover `.tare-tmp-` files. It holds the source's
profile locks (`ProfileLock::try_acquire`) for the walk and refuses if the destination target
already exists, so it can never merge into a live target. Index: for every copied file whose
source stamp the index knows, the destination stamp is stored with the same hash and `shared`,
and the source is marked shared, so the next dedupe leaves both alone; a file the index has not
seen stays unknown and costs one needless clone on the next dedupe — a known limit, not a hash
pass over gigabytes at seed time. Tests (`tests/seed.rs`, real `git worktree` + the cargo
fixture): what the first build in a seeded worktree actually rebuilds (measured, not assumed),
the excluded files, `--from`, refusal on an existing target, a busy source profile, the index
entries, and an A/B pair of identical worktrees where only one is seeded. Verify: `just check`.

Outcome: `src/seed.rs` + `cargo tare seed [--from <DIR>] [--dry-run] [--index <FILE>] [<DIR>]`.
`choose` takes the git common dir of the destination (`inventory::family`, now public), asks the
new `inventory::checkouts` for every checkout registered under it (the repository plus each
`worktrees/<name>/gitdir`) and looks in each at the *same relative path* the destination has
inside its own checkout — a workspace can sit anywhere in a repository, which the fixture proved
by having its workspace in `ws/` — then takes the target built most recently. The copy walks the
source with `walkdir`, recreates dirs and symlinks, `fs::copy`s files (`clonefile` on APFS, so
the new target shares every block and the volume loses nothing) and leaves behind
`incremental/`, `.cargo-lock` and `.tare-tmp-` leftovers. Every source profile dir is locked with
`ProfileLock::try_acquire` for the walk; one a build holds is reported and skipped whole (exit
code 2, as in `run`). A destination that already has a target is refused, never merged into.
Index: a copy whose source stamp the index knows is stored with the same hash and both sides are
marked shared, so the next dedupe leaves the pair alone.

Measured, not assumed: a seeded worktree rebuilds strictly less than an empty one, but not
nothing. Units whose absolute path is part of their fingerprint — the workspace members and the
path dependency — are compiled again in the new checkout. The fixture builds `--offline` from
path dependencies only, and registry dependencies are exactly the units that keep their paths
across worktrees, so the measured win is the floor of the real one. The card's "compiles
workspace members only" turned out to be optimistic and the A/B test states what actually
happens instead.

`tests/seed.rs`, 7 tests on a real `git worktree` of the cargo fixture: the copy is byte-identical
and its size is what the report claims; the cache and the lock files are left behind; a dry run
copies nothing and a second seed is refused; a busy source profile is reported and its dir not
copied; the source is chosen inside the family and both sides end up shared in the index; an A/B
pair where seeding is the only difference; and the CLI end to end. `tests/common/mod.rs` gained
`cargo_at` / `stale_units_at` so the oracle can run in any checkout. `just check` green.

Known limits: the seeded target weighs what `du` reports even though it shares every block —
only free space shows the truth (`docs/bench.md`); `--from` is not checked for belonging to the
same family; a file the index has never hashed costs one needless clone on the next dedupe.

### T10. Configuration and reporting

`~/.config/cargo-tare/config.toml` (roots, `min-age`, `min-size`, per-pass switches and thresholds,
family overrides), flag overrides, table and JSON reports, meaningful exit codes. Done: documented
in `README.md`, invalid config fails with a precise message. `run` without arguments takes the
configured roots (today it requires a `<ROOT>`).

Execution plan: `src/config.rs` — a serde `Config` read from
`$XDG_CONFIG_HOME/cargo-tare/config.toml` (else `$HOME/.config/...`), `deny_unknown_fields` and
kebab-case keys so a typo stops the run instead of silently doing nothing; keys `roots`,
`lossy`, `min-age`, `min-size`, `[evict] idle-days / max-total-gib`, `[incremental] idle-days`,
and `[family."<dir>"] skip` for leaving one repository alone. Only `skip` is per family: the
evict cap and the idle rules are decided over everything under the roots at once, so they stay
global — documented, not silently dropped. `--config <FILE>` points at another file (also what
the tests use); every flag wins over the file; `<ROOT>` becomes optional and falls back to
`roots`. Reporting: `run --json` prints one document (groups, busy dirs, per-pass counts,
removals with reasons, skips) built by a `#[derive(Serialize)]` view in `main.rs`, so the engine
types stay plain. Exit codes: 0 done, 1 error, 2 something was left busy — what a cron job needs
to tell the difference. Tests: unit tests on parsing and precedence, integration tests for a
config-driven run, a broken config naming its key, `--json` parsed back with serde_json, and
exit code 2 on a busy profile. Docs: `README.md` config section with a full example file,
`DESIGN.md` CLI surface. Verify: `just check`.

Outcome: `src/config.rs` — `Config::load` reads
`$XDG_CONFIG_HOME/cargo-tare/config.toml` (else `$HOME/.config/...`) with `deny_unknown_fields`
and kebab-case keys, so a typo names itself and stops the run instead of being ignored; a
missing file is the defaults, an unreadable or invalid one is an error naming the file. Keys:
`roots`, `lossy`, `min-age`, `min-size`, `[evict] idle-days / max-total-gib`,
`[incremental] idle-days`, `[family."<dir>"] skip`. `--config <FILE>` reads another file and
fails if it is not there. Every flag wins over the file (`Option::or` at each threshold, a
non-empty `--lossy` replaces the list). `<ROOT>` is now optional for `run` and falls back to
`roots`, with a precise error when both are empty; `status` takes the same `roots`, keeping `.`
as its fallback.

`skip` is the only per-family key, and the card's "family overrides" stop there on purpose: the
`evict` cap and both idle rules are decided over everything under the roots at once, so a
per-family threshold would be a lie. `indicatif` and `owo-colors` were approved for this task
and not used — nothing here needs a progress bar or colour yet.

Reporting: `run --json` prints one document (dry-run flag, groups with family, busy dirs, temps
removed, per-pass counts, every removal with its reason, every skip) from a `Serialize` view in
`main.rs`, so the engine types stay plain; the table print moved into `print_report`. Exit codes:
`0` done, `1` failed, `2` a profile dir was left alone because a build held its lock — `main`
now returns `ExitCode`.

Tests: 3 unit tests in `src/config.rs` (defaults, an unknown key naming itself, every key
parsed) and `tests/config.rs` with 7 more — the file supplying roots and the lossy pass, a flag
beating the file, an A/B pair where `skip = true` is the only difference between two identical
trees, a broken file naming itself and the key, a `--config` file that must exist, the JSON
report parsed back with `serde_json`, and exit code 2 on a busy profile. Every integration test
now runs the binary through `common::tare(config_home)`, which points `XDG_CONFIG_HOME` at a
temp dir: a test must never read the machine's configuration. `tests/cmd/run-needs-a-root.trycmd`
was dropped for the same reason (its subject is now our own error, and its outcome would depend
on the machine's config file); the case lives in `tests/cli.rs`. `just check` green.

Known limits: `~` in a config path is not expanded (a shell does it, a file does not); `roots`
are not de-duplicated; no `[compress]` / `[dedupe]` tables yet, `min-age` / `min-size` set both
passes at once as the flags do.

### T13. Lossy pass: `incremental`

Drop `<profile>/incremental/` in profile dirs nobody has built in for N days. Nothing in the
field does this: `cargo-clean-all` and `kondo` drop whole targets, and `CARGO_INCREMENTAL=0`
avoids the directory at the price of every rebuild everywhere. `docs/research.md` measured
`incremental = false` at −5.8 GB for one target (−40% of it) with local rebuilds 1.4–5× slower.
Keeping the cache for what you are working on and dropping it everywhere else takes the size
without the slowdown, and it is the largest single win still unclaimed after compress and dedupe.

Only workspace members are compiled incrementally, so the cost of dropping it is one
non-incremental rebuild of the workspace crates; third-party deps are untouched. Reuses the
removal machinery of `evict` (whole dir, under cargo's lock, re-checked after locking), with
`--lossy incremental --incremental-idle-days <N>`. Done: on the fixture the dir is gone and the
oracle shows the rebuild is limited to workspace members; a busy profile is never touched; the
reason is in the report on a dry run too.

Outcome: `src/incremental.rs` — pure `select(profiles, now, idle_days)` returns the profile dirs
that have an `incremental/` and whose last build is at least N days old (unknown last build is
never chosen, as in `evict`); the lossy `Incremental` pass re-checks under the lock that the dir
is still there and `inventory::last_built` still equals the inventory's reading. CLI:
`--lossy incremental --incremental-idle-days <N>`, which need each other; third in the pipeline,
after `evict`.

Engine: instead of a third removal variant, `Action::RemoveProfile` became `Action::Remove`,
which accepts a locked profile dir **or a dir inside one**, and its byte accounting sums the
inodes whose paths all lie under the removed dir (a link from outside frees nothing). `evict`
plans the same action unchanged; the profile's lock stays valid when only a subdir goes.

Measured, and better than the card assumed: dropping a real cache rebuilds **nothing**. The
cache is not part of cargo's fingerprint, so `Fixture::assert_fresh` passes right after the pass
(zero stale units, tests and binary still run). The price is one non-incremental rebuild of the
workspace members on the next edit — which is why the pass is meant for profiles you are not
working in.

`tests/incremental.rs`, 8 tests: idle cache goes while the artifacts, the lock file and a fresh
profile's cache stay; a profile without a cache is never planned; not run unless named, dry run
only lists; a running build is untouched; a build after the inventory keeps the cache; an A/B
pair of identical trees where the freed bytes equal exactly the control's cache; the real
fixture above; and the CLI end to end (both halves of the flag pair, dry run, removal). Two unit
tests cover `select`. `just check` green.

Known limits, as documented in `DESIGN.md`: the whole cache of a profile goes or none of it
(cargo's per-crate session dirs are not read); a busy profile is skipped; the pass's own yield
across a machine is not benchmarked yet — `docs/research.md` only has the −5.8 GB from building
one target with `incremental = false`.

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

Execution plan: benchmark a **copy** of a private 587-crate workspace (creator's choice) — `git archive`/`rsync`
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

Numbers (`docs/bench.md`, that workspace, 587 crates, two checkouts, two full runs): compress
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

### T14. Compress and report the cargo home

`~/.cargo/registry/src` holds every dependency's unpacked sources — plain text, the most
compressible bytes on the machine — and `git/checkouts` the same for git dependencies. No
competitor compresses them: `cargo-cache` and `cargo-trim` only delete, and cargo's own
`cache.auto-clean-frequency` (stable since 1.88) only evicts by age. Compression is lossless
here in the strongest sense: the files are a cache of immutable, re-downloadable sources.

Scope: `status` reports the cargo home's size next to the targets; `run` compresses
`registry/src` and `git/checkouts` when `--cargo-home` is given. Must take cargo's own lock
(`$CARGO_HOME/.package-cache`) for the length of the pass, the way the engine takes
`.cargo-lock` per profile, and must leave `registry/cache` (already compressed `.crate` files)
alone. Done: measured ratio in `docs/bench.md`, a `cargo build` after the pass does not
re-extract anything, and the pass refuses to run while another cargo holds the package lock.

Plan: `engine::run` gains a `Locks` argument — `PerDir` (today's behaviour, one `.cargo-lock` per
dir) or `Shared(path)`, one lock file for every dir at once, which is what
`$CARGO_HOME/.package-cache` is. `src/cargo_home.rs` finds the home (`CARGO_HOME`, else
`~/.cargo`), lists the dirs worth compressing (`registry/src`, `git/checkouts`, never
`registry/cache`) and inspects them for `status`. `run --cargo-home` then runs the existing
`compress` pass over those dirs as one more group in the report. Verify: `tests/cargo_home.rs`
over a fake home in a temp dir — an A/B with `--cargo-home` the only difference, `registry/cache`
untouched, every file byte-identical with its mtime after the pass (which is what decides whether
cargo re-extracts), and a held `.package-cache` leaving everything alone with exit 2. Bench on a
clone of a subset of the real home, never on the home itself.

Outcome: done. `src/cargo_home.rs` finds the home (`--cargo-home [DIR]` → `CARGO_HOME` →
`$HOME/.cargo`), lists the dirs worth compressing (`registry/src`, `git/checkouts`) and inspects
them for `status --cargo-home`, which stays opt-in because it costs a second full walk.
`engine::run` gained a `Locks` argument: `PerDir` is what every target group uses, `Shared(path)`
takes one lock for the whole group — `<home>/.package-cache`, the file cargo itself holds while
it fetches or extracts, since it writes no `.cargo-lock` there. A home cargo has never used (no
`.package-cache`) is refused rather than locked into existence, and `run --cargo-home` with no
roots at all is a valid run.

Only `compress` runs on the home: there is nothing to dedupe against and nothing stale to evict.
`registry/cache` (the `.crate` archives) and `registry/index` are left alone.

Measured (`docs/bench.md`, `scripts/bench-cargo-home.sh` / `just bench-home`, on an APFS clone of
the real home — never on the home itself): 1.52 GiB of registry sources → 469 MiB, −69.2%, in
44.5 s; `registry/cache` unchanged to the byte. The re-extraction criterion is answered twice:
the benchmark rebuilds a crate from the clone after the pass and cargo reports 0 units not fresh
with `.cargo-ok` unchanged in inode, mtime and size, and `tests/cargo_home.rs` asserts content
and mtime over every file in a fake home. `git/checkouts` was empty on this machine — that half
rests on the fixture test alone.

Tests (`tests/cargo_home.rs`, 6): the A/B with `--cargo-home` as the only difference, the packed
crates left alone, a held `.package-cache` giving exit 2 with nothing touched, a dry run
reporting the home as its own group with only `compress` in it, `status` measuring the home only
when asked, and a home without `.package-cache` refused.

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

Plan: `src/toolchains.rs` reads cargo's own fingerprints —
`<profile>/.fingerprint/<unit>/*.json`, whose `rustc` field is the hash of the compiler that
built that unit — and groups the units by it, newest first. No file name is parsed: the unit is
the directory, and the hash is a number cargo wrote. The inventory gets the counts per profile
(`ProfileInfo::toolchains`) and the target gets the totals plus an estimate of the bytes, which
is the profile's own size split by the share of units, because mapping a unit to its files needs
the layout only `R1` gives. `status` prints one line per target when more than one rustc appears,
`advise` adds the note, and both say nothing when there is only one.

Verify: `tests/toolchains.rs` over the real build fixture — build it, rewrite the `rustc` field
in half the fingerprints (which is what a toolchain upgrade leaves behind) and check the report
finds exactly those units, plus a pristine build reporting nothing at all. No A/B test: this task
adds no pass and changes nothing on disk.

Outcome: done as a report, which is where the card ended on purpose. `src/toolchains.rs` reads
`<profile>/.fingerprint/<unit>/*.json` and groups the units by the `rustc` hash cargo wrote
there, newest fingerprint first, so the head is the compiler in use and everything after it is
what an upgrade left behind. No file name is parsed anywhere: the unit is a directory, the
compiler is a number. `Target` gained `toolchains`, `stale_units` and `stale_bytes_estimate`;
`status` prints one line per target when a second compiler appears, `advise` adds a note that
points at `cargo clean`, and a target built by one rustc — the ordinary case — says nothing.

Honest about the number: the bytes are an estimate, and the field name says so. The profile
dirs' size is split by the share of the units, because a fingerprint names no artifact and the
exact map needs cargo's newer build-dir layout (roadmap `R1`). That is also why nothing here
deletes: `cargo-sweep --installed` can only do it by parsing hashed file names.

Tests: three unit tests over a handmade fingerprint tree (one compiler is no finding, the older
compiler's units are the stale ones, an unbuilt profile says nothing) and three integration
tests over the real build fixture, where the second toolchain is simulated the only honest way —
by rewriting the `rustc` hash in half the fingerprints and dating them back a month, which is
exactly the state an upgrade leaves. No A/B test: this task adds no pass and changes nothing on
disk.

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

Plan: the engine already compares whatever profile dirs one run is given and already sorts its
locks, so the whole change is in the grouping: `--across-families` (config `across-families`)
puts every target under the roots into one group instead of one group per family. The report
names that group `<across families>` rather than a family dir. Per-family `skip` still applies,
because it is decided before the grouping. Verify: `tests/across_families.rs` — two fixtures in
temp dirs of their own, each with the same file planted in its target the way an identical
third-party artifact looks, run as an A/B where the flag is the only difference: with it the two
files share an inode, without it they do not, and both builds are still fresh either way. Then
`scripts/bench.sh` gets the second measurement and `docs/bench.md` the number.

Outcome: done, and smaller than the card feared. The engine already compares whatever profile
dirs one run is given and already takes its locks in sorted order, so `--across-families`
(config `across-families`) changes only the grouping: one group for every target under the
roots, named `<across families>` in the report. Per-family `skip` still applies, because it is
decided before the grouping.

Measured (`scripts/bench.sh`, `WITH_ACROSS=1`, which adds a second **independent clone** of the
repository — its own `.git`, so its own family): a freshly built 351.8 MiB target, already
deduped inside its own family, gave up another **172.6 MiB** in 1.9 s once it was compared with
the other family, and neither checkout had a single unit go stale. About half of a new target
was already on the disk in a project that has nothing to do with it.

Honest about the benchmark: this one ran on `cargo-tare`'s own repository, not on the bigger workspace,
because the machine had 10 GiB free and three checkouts of it do not fit under the script's
free-space guard. The ratio is what the number is good for; the absolute sizes are an order of
magnitude smaller than the other benchmarks in `docs/bench.md`.

It stays opt-in, and the reason is in the same numbers: the run holds every target's build locks
for its whole length, which on the 587-crate workspace is over a minute of no builds anywhere.

Tests (`tests/across_families.rs`): the A/B with two real fixtures in temp dirs of their own —
neither has a repository, so each is its own family — each holding the same planted artifact,
with the flag as the only difference. With it the second copy's inode is replaced (a clone is a
new inode sharing the old one's blocks, which is what an inode check has to assert) while its
bytes and its mtime are not; without it nothing moves; and all four builds are still fresh
afterwards. A second test checks the report names one group instead of two.

### T19. Platform layer: build and run on Linux and Windows

The tool was macOS-only, and not by design: `model.rs`, `engine.rs`, `seed.rs` and the two
lossless passes reached for `std::os::unix::fs::MetadataExt` (`dev`, `ino`, `nlink`, `blocks`,
`st_flags`) and for macOS's `clonefile` and `UF_COMPRESSED` directly. Windows has none of those
names, so the crate did not compile there at all.

Blocker for T20 and T21: `src/sys/` now owns every platform primitive — `file_id`, `nlink`,
`allocated`, `flags`, `mode`/`set_mode`, `symlink`, `clone_file`, a `Compressor` — plus the two
capability constants `CAN_CLONE` and `CAN_COMPRESS`, with one file per platform picked by
`#[cfg_attr(..., path = ...)]`. macOS kept exactly what it had; `applesauce` became a macOS-only
dependency. `Dedupe::plan` and `Compress::plan` return an empty plan when their capability is
false, before reading a single file: a clone the filesystem cannot share is a second copy of the
bytes, and a compress pass with no backend would clone every candidate only to throw the copy
away.

Outcome: `cargo check` is green for `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`
(`just check-cross`), the macOS suite is unchanged and green, and no `std::os` import is left in
`src/` outside `src/sys/`. Three unit tests in `src/sys/mod.rs` state the facts that must hold on
every platform (a file has an identity of its own and a size on disk, a clone holds the bytes of
its source, an empty batch costs nothing), so they are what a port has to satisfy; the
integration suite still runs only where the machine is.

Honest about what the other two platforms do today: nothing but report. Both capabilities are
false on Linux and Windows, because a clone there depends on the filesystem under the root
(btrfs and XFS reflink, ext4 does not; ReFS clones, NTFS does not) and that is a runtime probe,
which is T20 and T21. Windows takes file identity from the path, so hardlinks read as separate
files and the link count always reads 1 — consistent within the model and inert while nothing is
planned, replaced by `GetFileInformationByHandle` in T21; sizes there are logical, not on-disk,
until `GetCompressedFileSize`. `seed` works on all three: where blocks are shared the copy is
free, where they are not it costs the disk and still saves the build. The test suite stayed
macOS-only, so the tests in `src/sys/` are a contract for the ports rather than proof they run.

### T20. Linux: reflink dedupe and filesystem compression

With T19 in place, fill in the Linux half. Dedupe: `FICLONE` (btrfs, XFS with reflink=1, bcachefs)
is the exact equivalent of `clonefile`; `FIDEDUPERANGE` is the safer variant that verifies the
bytes in the kernel and works even when the target is shared already. `rustix` is already a
dependency and covers both, so no new crate should be needed. Compression: btrfs takes
`chattr +c` (`FS_COMPR_FL`) per file, and only new writes are compressed, so a file has to be
rewritten to shrink — which is what the pass does anyway. ext4 has neither, so both passes must
report "not supported here" rather than pretend.

Done: the pass suite runs on a btrfs loopback image in CI, `ext4` falls back to T22 instead of
failing, and `docs/bench.md` gains a Linux row.

Plan: the capability stops being a constant. `sys::caps(dir) -> Caps { clone, compress }`, cached
per `st_dev`, answers what the filesystem under a profile dir can actually do, and both lossless
passes filter their profiles by it — which is also the answer to "report, do not pretend". On
Linux the probe is empirical rather than a filesystem-name table: two temp files and one
`FICLONE`, one `FS_IOC_SETFLAGS` with `FS_COMPR_FL`, both cleaned up. That gets XFS with
`reflink=0`, btrfs mounted `nodatacow` and a bind-mounted ext4 right, which a name table does
not. macOS keeps `true/true` (APFS is what it was measured on), Windows stays `false/false`
until T21.

`clone_file` on Linux becomes `FICLONE` through `rustix` instead of `fs::copy`, so a filesystem
that cannot share blocks fails loudly here instead of silently copying them. The compressor sets
`FS_COMPR_FL` on the engine's private copy and rewrites it through itself, because btrfs
compresses new writes only; `flags` starts reading `FS_IOC_GETFLAGS` so the engine's
"came back compressed" check works, and `st_blocks` then shows the win.

`FIDEDUPERANGE` is deliberately not used: the engine already replaces whole inode groups
atomically, re-checks every stamp under cargo's lock and restores mode and mtime, so an
in-place dedupe would be a second apply path with the same invariants to maintain and nothing
the first one does not already give. Say so if that call is wrong.

Verify: `just check` and `just check-cross` here, then a Linux VM (lima) with a btrfs loopback
image for the real suite, per the creator's choice of "compile first, then a VM". Nothing is
claimed to work on btrfs before that VM has run it.

Outcome: done, and two of the plan's own claims above turned out wrong — both found by running
it rather than reading it.

`sys::caps(dir) -> Caps { clone, compress }` landed as planned, cached per `st_dev`, and both
lossless passes filter their profiles by it before reading a file. `status` prints what the
filesystem cannot do under each target, and the inventory no longer counts savings it cannot
deliver: `compressible_bytes` and `dedupe_candidate_bytes` are zero where the capability is
missing, in the JSON as well as the text.

What the plan got wrong:

1. **The compression probe cannot be an attempt.** ext4 accepts `FS_IOC_SETFLAGS` with
   `FS_COMPR_FL`, keeps the flag where `lsattr` shows it, and compresses nothing — 200 MiB
   written with it set took 200 MiB. Measuring the file afterwards does not rescue the probe
   either, because btrfs reports the *uncompressed* size in `st_blocks`. Compression is now
   decided by `statfs().f_type == BTRFS_SUPER_MAGIC`; cloning stays a real `FICLONE`, which does
   tell the truth.
2. **`st_blocks` does not show the win on btrfs.** The plan said it would. A measured run
   compressed 1553 files and printed `applied 1553 (0 bytes)` while the volume gained 818 MiB —
   the pass worked, the platform cannot report it. `sys::ALLOCATED_SHOWS_COMPRESSION` says which
   platform is which, the A/B tests branch on it, and `docs/bench.md` has the Linux table with
   the free-space column marked as the only one to read there.

One real bug came out of the VM that no amount of macOS testing would have found: the capability
probe created and removed temp files inside the directory it probed, which moved that directory's
mtime — the same mtime `evict` and `incremental` read to tell an idle profile from a busy one.
Every target looked freshly built and `incremental` quietly planned nothing. The probe now puts
the mtime back, and `sys::tests::the_probe_cleans_up_after_itself` fails if it ever stops.

Verified: `just check` and `just check-cross` on macOS, the full suite green on macOS, and in a
lima VM (Ubuntu 24.04) on two loopback images — btrfs: 21 suites green; ext4: 21 suites green,
with `caps` finding neither capability and both passes planning nothing. Tests that can only be
observed where blocks are shared use `common::filesystem_can`, which returns early with a line on
stderr; the other side of each is asserted in `tests/caps.rs`, which runs the same fixture and
the same passes on both filesystems. One flake seen once under full parallel load on the ext4
image (`harness::oracle_is_green_on_an_untouched_build_and_sees_a_changed_mtime`, green alone and
green on the next full run) — fixture build timing in a 4-core VM, not a pass.

Not done, and deliberately: the suite runs in a local VM, not in CI — the creator chose
"compile first, then a local VM" over GitHub Actions. `ideas.md` carries the CI job as an idea.
`FIDEDUPERANGE` stays unused for the reason the card gives; nothing measured here changed that.
ext4 still wins nothing, which is T22.

### T22. Link fallback where the filesystem cannot clone

ext4 and NTFS have no copy-on-write, so `dedupe` has nothing to plan there. The fallback is a
hardlink, and the reason it is not simply the default is a real hazard: rustc opens its output
files with truncate, so a rebuild rewrites the inode in place and would rewrite every other name
pointing at it. Cargo's own hardlinks (`target/debug/fx` to `deps/fx-<hash>`) are safe because
cargo replaces the name rather than the inode; ours would not be.

So the fallback is split by what the file is, not by what the filesystem allows:

- **Cargo home sources** (`registry/src`, `git/checkouts`): safe to hardlink. Cargo extracts a
  crate into a fresh directory and writes `.cargo-ok` last; it never rewrites an extracted file
  in place. This is where the bytes are anyway — 1.52 GiB of them on the machine measured in
  `docs/bench.md`.
- **Build artifacts in a target dir**: only behind an explicit flag (`--link-artifacts`), with
  the truncate hazard in `--help`, in the README and in the dry-run output. Nothing enables it
  for the user.

Compression is unaffected and keeps its own answer per platform: APFS and btrfs and NTFS have
it, ext4 and XFS do not, and a pass that cannot run says so instead of failing. Done: a fixture
on a filesystem without reflinks shares the cargo home's sources and leaves target artifacts
alone unless the flag is given, `status` says which of the two the filesystem under each root
can do, and the hazard is written down where a user meets it.

Plan followed: `Replace` gained a `how: Share` field — `Share::Clone` or `Share::Link` — so the
engine is told how to share rather than guessing, and `Dedupe::share(profile)` answers it per
profile: a clone wherever `caps.clone` is true, a hardlink where it is not *and* the pass was
given `link_fallback`. `run --cargo-home` now runs `dedupe` beside `compress` with that fallback
on; target groups get it only from `--link-artifacts`.

Outcome: done. Two rules turned out to belong in the engine rather than the pass, because both
are about the mechanism and not about the policy:

- **Modes must already agree.** One inode holds one mode, so linking files whose permissions
  differ would quietly change the other name's. That is `Skip::ModeMismatch`, and nothing is
  touched when it fires.
- **The shared inode keeps the later of the two modification times.** A hardlink hands the
  member the source's mtime, and a file that suddenly reads older than what it was built from is
  a file cargo rebuilds — the pass would then cost a build instead of saving space. The source's
  own mtime moves forward with it, which is the safe direction.

`status` says the whole truth now: on a filesystem with neither capability the line reads
*compress finds nothing here, dedupe only links cargo home sources* rather than claiming both
passes are idle.

Tests: `tests/link.rs` is the A/B, and the flag is the only difference between its two runs. Both
filesystem outcomes are stated in each test rather than skipped, as in `tests/caps.rs` — where
blocks can be shared the twin is cloned and the flag changes nothing, where they cannot the
control run leaves the artifacts exactly as it found them and only the treatment shares them.
The cargo home's sources are shared with no flag on either side. Both runs end with the freshness
oracle, because linking moves mtimes and a pass that costs a rebuild is not a saving.
`tests/engine.rs` covers the mechanism itself on every platform (one inode for the whole group,
the later mtime kept, a mode mismatch refused), which is what keeps the link path under test on
macOS, where it is never taken by policy.

Verified: `just check`, `just check-cross` and the full suite on macOS (22 suites), and in the
lima VM on btrfs and ext4 loopback images, all green — on ext4 the link path is the one actually
taken. Two flakes were seen there under full parallel load, each once, each green alone and on
the next full run (`harness::oracle_is_green_…` during T20, `doc::ab_only_the_named_run_removes_
the_docs` here): fixture builds racing on 4 cores, not a pass.

Not done: nothing from the card. `docs/bench.md` gained no row for this — the VM has no real
cargo home to measure and pointing the tool at the machine's own is not something a test or a
benchmark here may do.

### T23. User guide, ecosystem study and T21 readiness analysis

Asked for directly by the creator, so it went from request to done without a stop in
`roadmap.md`. Documentation only; no code, manifest or dependency changed.

- `docs/usage.md` — install, the first five minutes, which passes are lossless and which delete,
  every command and option, exit codes, recipes, configuration, troubleshooting. The option
  tables were written from the binary's own `--help` output.
- `docs/ecosystems.md` — a desk study of whether the passes fit C / C++, .NET, Go, Swift / Xcode,
  content-addressed stores, the JVM and Bazel: what in the codebase is cargo-specific, the six
  questions an adapter has to answer, a verdict per pass per ecosystem, the existing tools and
  the gap they leave. Unverified claims are marked; nothing was measured. The follow-up is in
  `ideas.md`, not approved.
- `README.md` — the stale "Planned: `cargo tare advise`" block is gone (it has worked since
  T12), the status line says what works where and points at both documents.
- `plan.md` — T21's card gained a readiness analysis: eleven open points, the first of which
  (where Windows tests run) is the creator's decision and the task's real blocker.

Verified: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
(127 passed on macOS) and both `check-cross` targets were green on macOS before the documents were
written; the documents change nothing those commands read.

### T42. Rename the project: `cargo-tare` becomes `dunnage`

Asked for by the creator: a name that fits a tool no longer tied to cargo, and that nobody
holds. `tare` itself is taken on crates.io. `dunnage` — the loose packing stuffed around the
cargo in a hold: it takes up room and is not the goods — keeps the metaphor and was free on
crates.io, Homebrew (formula and cask), npm and PyPI when checked, with only zero-star
repositories of that name on GitHub. Runners-up that were also free: `unladen` (crates.io and
Homebrew only), `plimsoll` and `freeboard` (both taken on npm and PyPI; `freeboard` is a
6.5k-star project).

What changed: the package, the library crate (`dunnage`) and the binary (`dunnage`, invoked
directly instead of as `cargo tare`); the config and cache dirs (`~/.config/dunnage`,
`~/.cache/dunnage` — neither existed under the old name on the creator's machine, so nothing
was migrated); the temp prefix (`.dunnage-tmp-`), the hash index magic, the launchd label, the
bench scripts, every living document and `rust.md`. `cargo dunnage <args>` still works through
a `cargo-dunnage` link to the binary: cargo passes the subcommand name first and `main` drops
it, which a new test in `tests/cli.rs` holds. Left alone on purpose: `done.md` above this entry
and `docs/spike/`, which are records of what was run under the old name.

The creator renamed the GitHub repository to `listepo/dunnage` (`gh repo rename`, which also
moved `origin`); `plan.md` and `docs/usage.md` carry the new URL. The local directory name is
the creator's to change.

Verified on macOS: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (128 passed, one of them new) and both `check-cross` targets.
