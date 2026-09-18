# cargo-tare — design

Tare: the weight of the packaging, not the goods. `target/` is packaging.

`cargo-tare` shrinks Cargo build directories without slowing builds down, by combining several
independent approaches in one planner instead of chaining separate tools. Measurements behind
every choice are in `docs/research.md`.

## Goals

- Cut on-disk size of many `target/` dirs (the measured case: 44 dirs, ~158 GB; 30 of them,
  113.9 GB, sit in worktree checkouts that git no longer registers and were last built a week
  earlier; the rest are live and built daily).
- Never make cargo rebuild anything it would not have rebuilt anyway.
- Make new worktrees cheap in both disk and build time.
- Be independent of cargo's internal directory layout.
- Re-runs after a build take seconds, not minutes.

## Non-goals

- Cleaning `~/.cargo` (cargo does it itself since 1.88; `cargo-cache` / `cargo-trim` exist).
- Speeding up compilation itself (sccache, hakari, profile tuning are orthogonal; `advise` only
  points at them).
- Replacing cargo features once they ship (target GC, per-user cache): overlapping passes retire.

## What the measurements say

| Approach | Measured size effect | Build-time cost | Decision |
| --- | --- | --- | --- |
| Age-based sweep in active targets | ~50 MB of 14 GB | none | not a pass; useless here |
| Transparent APFS compression | deps to ~20–30% of logical size | small CPU on read | **core pass** |
| Content dedupe via `clonefile` | 36% across sibling targets, 33% inside one `deps/`, 3% for a diverged worktree | none | **core pass** |
| Clone-seeding a new worktree's target | 0 bytes until divergence | removes the third-party rebuild | **core command** |
| Whole-dir removal of orphaned / idle targets | **113.9 GB of ~158 GB** (30 unregistered worktree checkouts, idle 6–7 days) | rebuild on return | opt-in pass, highest payoff on the measured machine |
| Unit-level prune (cargo-gc style) | unmeasured (~3 GB of incremental variants seen) | needs a build | roadmap, needs layout v2 |
| Shared `build-dir` per repo | est. 20–35% | parallel agents serialize on one lock; workspace members get the same unit hash in every worktree (T2), so worktrees overwrite each other's member artifacts | `advise` only; automation on roadmap |
| Symlinked / shared `target` | same as shared `target-dir` | one lock, binaries overwrite each other | rejected |

## Core model

1. **The unit of work is an inode, not a path.** A single target already holds ~71k hardlinked
   files (rustc links `incremental/` ↔ `deps/` ↔ profile root). Every mutation replaces an inode
   for *all* of its paths or not at all. A group whose link count exceeds the paths we found is
   skipped.
2. **Layout independence.** Passes look at directories, inodes and content. No pass parses
   `name-<hash>` file names, so `target-dir`, `build-dir` and build-dir layout v2 all work.
   The only layout knowledge: a cargo `CACHEDIR.TAG` (checked by its cargo-specific text, because
   gradle / uv / huggingface caches carry the tag too) and profile dirs holding `.cargo-lock`.
3. **Families.** Target dirs whose projects share a git common dir (worktrees) or a remote form a
   family. Dedupe candidates and seeding sources are searched inside a family first, which is where
   the measured duplication is.
4. **Lossless vs lossy.** `seed`, `dedupe`, `compress` never lose data and are on by default.
   `orphans`, `evict` (and later `prune`) delete rebuildable data and are off until configured.

## Pipeline

`scan → plan → apply → report`. One walk, one model, one lock acquisition per target.

| Order | Pass | Kind | What it does |
| --- | --- | --- | --- |
| 0 | `seed` (command, at worktree creation) | lossless | clone a family member's target into the new worktree |
| 1 | `orphans` | lossy | targets whose project dir / worktree is gone or whose branch is merged |
| 2 | `evict` | lossy | whole profile dirs idle for N days; least-recently-built first until under a global size cap |
| 3 | `incremental` | lossy | the `incremental/` cache of profile dirs idle for N days |
| 4 | `doc` | lossy | `<target>/doc`, what `cargo doc` writes and no build reads |
| 5 | `prune` | lossy | roadmap: units the current build graph no longer references |
| 6 | `dedupe` + `compress` (fused) | lossless | see below |
| 7 | `report` | — | bytes before / after per pass, per target, JSON or table |

Ordering rule: delete first so lossless passes never hash or compress bytes that are about to
disappear; lossless passes last so they see the final set of inodes.

One run does not reach a fixed point: dedupe's clones are files compress never saw, so the next
run finds them (46 actions on the benchmark workspace, `docs/bench.md`). Nothing is lost by it;
a `run` that repeats until it finds nothing would finish in one go.

### Why one tool beats a chain of tools

- **Fused dedupe + compress.** Chained tools fight: compressing after dedupe rewrites each clone
  and un-shares it; deduping after compressing re-hashes everything on every run. The planner
  instead groups inodes by content, compresses **one** canonical inode per group, then clones the
  compressed canonical over the other members. T2 confirmed both halves: compressing a clone
  *increases* usage (+16 MB on a 52 MB target, the rewrite un-shares it), while cloning a
  compressed file shares blocks and keeps the compressed flag (−14.5 MB of −16 MB expected).
- **Shared hash index.** Content hashes are cached by `(device, inode, size, mtime)`. After a
  build only new inodes are hashed. Hashing is also skipped for any file whose size is unique in
  its family (size-bucket prefilter).
- **Hot-file filter.** Inodes younger than `min-age` (default 1 h) are left alone. Workspace crates
  and incremental state are rewritten by the next build; compressing them is wasted CPU. Third-party
  artifacts are stable for weeks and carry most of the bytes.
- **One inventory, one lock.** Every pass reads the same scan and mutates under the same cargo
  lock, so a build never observes a half-applied state.

### Pass details

**compress** — per-file transparent APFS compression (decmpfs). Skips files that are already
compressed, smaller than 8 KB, or younger than `min-age`. Backend: the `applesauce` library
(LZFSE). In T2 its CLI kept all hardlink groups, mtimes and modes and left every unit fresh.
`ditto --hfsCompression` compressed nothing on the test volume and is not used.

**dedupe** — `clonefile(2)` of the canonical inode to a temp name in the same directory, restore
the member's mtime / mode / flags, then `rename(2)` over each path of the member's hardlink group.
Clones are copy-on-write, so a later in-place write by rustc cannot leak into siblings.

**seed** — `cargo tare seed --from <worktree> [<new-worktree>]`, or automatic source selection
inside the family (largest recently built target). Recursive clone of the target dir, excluding
`incremental/` and lock files. Verified in T2: registry dependencies are fresh in the new
worktree, only workspace members rebuild (their sources have new mtimes; their unit hashes are
the same in every worktree). Preserving mtimes during the clone is not required. Seeding from an
already compressed target keeps both the sharing and the compression.

**orphans / evict** — operate on whole target or profile dirs only, which is layout-proof and
needs no knowledge of units. "Last built" is the newest mtime among the profile dir's top-level
entries.

## Worktrees: clone-seed vs symlink vs shared build-dir

| | Symlinked `target` | Shared `build-dir` (cargo ≥ 1.91) | Per-unit symlinks into a store | Clone-seed + dedupe |
| --- | --- | --- | --- | --- |
| Third-party deps built once | yes | yes | yes | yes |
| Disk shared | yes | yes | yes | yes (CoW) |
| Build lock | one for all worktrees | one for all worktrees | per worktree | per worktree |
| Final binaries collide | **yes** — wrong binary gets tested | no | no | no |
| Cross-worktree corruption | possible | cargo-managed | **yes**, cargo writes through the link | impossible |
| Needs | nothing | nothing | layout v2 | APFS / reflink fs |
| `cargo clean` blast radius | everything | everything shared | one worktree, store leaks | one worktree |

Decision: clone-seed + dedupe is the default on reflink filesystems. Shared `build-dir` is offered
by `advise` for users who do not build in parallel. Symlink modes are kept only as a roadmap item
for filesystems without reflinks.

## Safety invariants

1. Hold cargo's own lock (`<profile>/.cargo-lock`, exclusive `flock`) while mutating a profile dir;
   `try_lock`, and skip the dir when a build holds it. Cross-target operations take locks in sorted
   path order.
2. Re-check `(size, mtime)` of source and member immediately before replacing; any change aborts
   that group.
3. Replacement is always temp-file + `rename` inside the same directory; a crash leaves either the
   old or the new file, plus at most a `.tare-tmp-*` file that the next run removes.
4. mtime and mode of every replaced path are preserved. T2: a workspace-member rlib with a new
   mtime makes its dependents rebuild; registry artifacts are not mtime-checked, but the rule is
   applied to everything. BSD flags: the compressed flag follows the content (a clone of a
   compressed file is compressed); an inode carrying any other flag is skipped, not rewritten.
5. Lossy passes never run unless enabled in config or by flag, and always support `--dry-run`.
6. Never follow symlinks out of a target dir; never cross a device boundary.

**Freshness oracle** (the acceptance test for every lossless pass): build a fixture workspace, run
the pass, then `cargo build --message-format=json` must report every unit as `fresh` and
`cargo test` must pass. Size is measured in allocated blocks (`st_blocks`), not logical length.

The harness is `tests/common/mod.rs`, shared by every integration test:

- `Fixture` — a workspace in a temp dir: a bin + lib package with a build script that writes into
  `OUT_DIR`, unit and integration tests, a proc-macro member, and a path dependency outside the
  workspace standing in for a third-party crate. It builds `--offline`, so tests need no network;
  a registry crate differs only in how cargo fingerprints its sources, which no pass touches.
  One source tree can be built into several target dirs (sibling targets for dedupe).
- `Fixture::stale_units` / `assert_fresh` — the oracle: the JSON messages of `cargo build` and
  `cargo test --no-run` must all say `fresh`, then `cargo test` and the binary must succeed.
- `allocated_bytes` — size of a dir the way the engine counts it, every inode once.
- `run_unbusy` — repeats an engine run that reads as busy because of the fork race between test
  threads (see "Engine") and sums the attempts: with several dirs, an attempt that found one busy
  has still worked on the others. The race is real — about one in seven rounds of
  `tests/compress.rs` hit it — and cannot happen in the tool itself, which spawns no processes.

`tests/harness.rs` proves the oracle can fail: after the mtimes of the `.rlib` files are bumped,
it reports stale units. A new lossless pass gets one test of the shape "build the fixture, run the
pass, `assert_fresh`". CLI tests follow `rust.md`: full output in `tests/cmd/*.trycmd`, exit codes
and messages with paths through `assert_cmd` + `predicates` in `tests/cli.rs`.

## Engine (`src/engine.rs`, `src/model.rs`)

`engine::run(profile_dirs, passes, options)` is the only code that mutates a target.

1. **Lock.** Sort and dedupe the profile dirs, `try_lock` each `.cargo-lock`. A dir held by a
   build goes to `report.busy` and is not even scanned. Locks live until `run` returns.
2. **Scan.** `model::scan` turns each locked profile dir into `Inode`s: `Stamp`
   (`dev`, `ino`, `size`, `mtime`), mode, flags, link count, allocated bytes and every path found.
   Symlinks are not followed, other devices are not entered, `.cargo-lock` is left out, and
   `.tare-tmp-*` leftovers are collected and removed (not on `--dry-run`).
3. **Plan.** Each `Pass` gets the scanned profiles and returns `Action`s without touching the
   disk. A lossy pass is asked only when named in `Options::lossy`. After a pass that applied
   anything the profiles are rescanned, so the next pass sees the new inodes.
4. **Apply.** `Action::Replace { source, source_stamp, member }` swaps every path of `member` for
   a copy-on-write clone of `source`: clone to a temp name next to the first path, restore the
   member's mtime and mode, `rename` over it; every other path gets a hardlink to the new inode
   through its own temp name and `rename`, so the group stays one inode. The engine trusts the
   pass that the bytes are equal and checks everything else.
5. **Report.** Per pass: planned and applied groups, allocated bytes (an upper bound on the
   saving), and every skipped group with its reason.

A group is skipped, never half-done, when: a path is outside the locked profile dirs
(`Unlocked`), the inode has links we did not find (`ForeignLinks`), it carries flags other than
the compressed one (`Flags`), source and member differ in device or size or are the same inode,
any stamp differs from the scan (`Changed`), or an I/O call fails (`Failed`). A crash in the
middle of a group leaves each path on the old or the new inode; both hold the same bytes, and the
next run joins them again.

`model::profile_dirs` maps a target dir to its profile dirs and refuses a dir whose
`CACHEDIR.TAG` was not written by cargo.

After a group is replaced the engine calls `Pass::replaced(replace, new_stamp)`, so a pass can
keep its own bookkeeping about the inode that now sits at the member's paths.

## Dedupe pass and hash index (`src/dedupe.rs`, `src/index.rs`)

Works across every profile dir given to one run; give two worktrees' targets together to share
files between them.

1. **Filter.** An inode takes part when it is at least `min-size` (default 4096 bytes, one APFS
   block), at least `min-age` old (default 1 h), has no links outside its profile dir and no
   flags other than the compressed one. The same rule holds for a source.
2. **Size buckets.** Inodes are bucketed by `(device, size)`. A size that occurs once has no twin
   and is never read; on a real target this removes most of the I/O.
3. **Hash.** SHA-256 of the remaining inodes, in parallel (`rayon`), the index first. Reading a
   transparently compressed file yields its plain bytes, so a compressed and a plain copy match.
4. **Group and choose.** Equal `(device, hash)` forms a group. The canonical inode is, in order:
   one already marked shared, one that is compressed (its clones stay compressed — this is the
   fusion with the compress pass, which only has to run first and compress canonicals and unique
   files), the oldest, the first path. Every other inode of the group that is not marked shared
   becomes a `Replace` action.
5. **Remember.** In `replaced` the source is marked shared, the member's old entry is dropped and
   the new inode is stored with the known hash and the shared mark — no rehash, and the next run
   plans nothing for it.

The **index** maps `(device, inode)` to `(size, mtime, hash, shared)`; a lookup with a different
size or mtime misses, so a rewritten file is rehashed and loses its shared mark. It is one flat
file of fixed little-endian records behind a magic string (`~/.cache/cargo-tare/hashes-v1.bin`,
`--index` to override), written through a temp file and `rename`, saved on `--dry-run` too. It is
only a cache: a missing, truncated or foreign file reads as empty.

Why the `shared` mark exists: APFS cannot be asked whether two files share blocks, and a clone is
a different inode with equal content — without the mark every run would clone everything again
and report savings that are not there. Known limits: two clusters shared by separate runs are not
merged with each other; a target seeded by `cp -c` or `cargo tare seed` is unknown to the index
and is cloned once more on its first run (T8 can register seeded inodes); losing the index costs
one full rehash and one redundant round of cloning; like cargo itself, the index trusts
`(size, mtime)`, so a file rewritten with the same size within the same nanosecond timestamp
would keep a stale hash.

## Compress pass (`src/compress.rs`)

The backend never sees a live file. `applesauce` refuses files with more than one link and
replaces the inode it compresses — both wrong inside a target, where final artifacts and all of
`incremental/` are hardlink groups. So compression is an engine action, `Action::Compress(inode)`:

1. The engine checks the group as for a replacement (locked paths, no foreign links, no flags but
   the compressed one, stamps unchanged) and clones its first path into a sibling temp. A clone
   costs no space.
2. Up to 256 such private copies go to `Pass::compress` in one call; the backend compresses them
   in parallel (LZFSE, level 5, keep only below 95% of the size, verify by reading back).
3. A copy that came back with the compressed flag and the same length is swapped in after the
   group's stamps are checked once more: mtime and mode restored by the engine, `rename` over the
   first path, `hard_link` + `rename` for the others — the group stays one inode. Any other copy
   is removed and the group reads `NotCompressed`; the backend's reason is kept in
   `Compress::notes`.
4. `Pass::rewritten(old, new)` tells every pass that the content moved to a new, unshared inode;
   dedupe moves its index entry, so the file is not hashed again.

The pass plans inodes of at least 8 KB, older than `min-age`, with no flags (so not compressed
yet) and not marked shared in the hash index: compressing a clone un-shares it. It runs before
dedupe, which prefers a compressed canonical, so its clones are compressed too — after one run a
family holds one compressed copy of each file. `freed_bytes` is allocated bytes before minus
after, measured, not estimated.

Known limits: duplicates are all compressed before dedupe folds them, which costs CPU, not
space; a cluster that was shared while uncompressed (a run with only dedupe) stays uncompressed;
a copy the backend refuses is tried again on every run; a file whose mode denies its owner
reading or writing ends as `NotCompressed` or `Failed`.

## Seed command (`src/seed.rs`)

Not a pass: it runs on its own, before there is anything to shrink.

- **Source choice.** `--from` names a checkout or a target dir. Without it, `choose` takes the
  git common dir of the destination (`inventory::family`), asks `inventory::checkouts` for every
  checkout registered under it (the repository itself and each `worktrees/<name>/gitdir`), looks
  in each one at the same relative path the destination has inside its own checkout — a
  workspace can sit anywhere in a repository — and takes the target built most recently.
- **The copy is a clone.** `fs::copy` is `clonefile` on APFS, so the new target shares every
  block with the old one and the volume loses nothing. Dirs are recreated, symlinks are
  recreated as symlinks, and `incremental/`, `.cargo-lock` and leftover `.tare-tmp-` files are
  left behind: a cache of another checkout's build, a lock that is not ours, and rubbish.
- **Under the source's locks.** Every profile dir of the source is locked with
  `ProfileLock::try_acquire` for the length of the walk; one that a build holds is reported and
  skipped whole, so nothing half-written is ever copied. Exit code 2, as in `run`.
- **Never into a live target.** A destination that already has a target dir is refused: seeding
  merges nothing.
- **The index.** For every copied file whose source stamp the index knows, the copy is stored
  with the same hash and both sides are marked shared, so the next dedupe leaves the pair alone.
  A source the index has never hashed stays unknown — seeding must not read gigabytes to fill an
  index that the next `run` fills anyway; the cost is one needless clone for that file.

Known limits: the yield depends on what moved — units whose absolute path is part of their
fingerprint (workspace members, path dependencies) are compiled again in the new checkout, and
the test suite measures this against an empty target instead of assuming it; the fixture has no
registry dependencies, which are exactly the units that keep their paths across worktrees, so
the measured win is a floor; `--from` is not checked for being in the same family.

## Orphans pass (`src/orphans.rs`)

Lossy, so it runs only with `--lossy orphans`. No threshold: an orphan either is one or is not.

- **What an orphan is.** A project whose `.git` file points at a worktree record
  (`<common dir>/worktrees/<name>`) that no longer exists — the repository was deleted, moved,
  or the record removed by hand. The inventory already reports it (`Target::orphaned`).
- **Only `target/` goes.** The checkout next to it stays untouched. Such a checkout can hold
  uncommitted work that git can no longer report, and `target/` is the only part of it that is
  rebuildable.
- **The whole target, not its profiles.** `Action::RemoveTarget` takes `doc/`, `package/`,
  `tmp/` and `CACHEDIR.TAG` with the profile dirs; nothing is left for the next run to find.
- **Re-checked under the lock,** the way `evict` re-reads the last build: the pass plans a
  removal only for a target that holds at least one dir the engine has locked and that
  `inventory::is_orphaned` still calls an orphan. A `git worktree repair` between the inventory
  and the lock keeps the target.
- **The engine removes, not the pass.** `RemoveTarget` is refused (`Skip::Unlocked`) unless we
  hold a lock inside the target and no dir under it is busy, so a target with a running build is
  never touched. The removed dirs leave the set later passes scan.
- `orphans` is first in the pipeline: no point evicting or compressing inside a target that is
  about to go whole.

Known limits: a target with one busy profile is kept entirely, even when its other profiles are
free; the removal is not atomic, so an interrupted run can leave a half-removed target, which
the next run finishes; a checkout whose repository is merely unreachable (an unmounted volume
holding the common dir) reads as an orphan — the `.git` file points at a path that does not
exist, and nothing else distinguishes the two.

## Evict pass (`src/evict.rs`)

Lossy, so it runs only with `--lossy evict`, and only together with at least one limit.

- **Selection is global and pure.** `evict::select(profiles, now, limits)` sees every profile dir
  under the roots (`ProfileInfo` from the inventory: dir, allocated bytes, last build) and returns
  the dirs to remove, each with its reason. First the idle rule: last build at least
  `idle_days` old. Then the cap: while the rest exceeds `max_total_bytes`, the least recently
  built goes next (ties broken by path, so a run is reproducible). Idle removals count toward
  the cap. A profile whose last build is unknown is never chosen.
- **The choice is made before any lock is held**, so the pass re-checks under the lock: it plans
  `Action::Remove` only for a dir the engine has locked and whose last build still equals
  the inventory's. A build that slipped in between keeps its profile.
- **The engine removes, not the pass.** `Remove` is refused (`Skip::Unlocked`) unless the dir is
  a locked profile dir or inside one; then the whole dir goes, lock file included,
  which is what `cargo clean --profile` does. The dir leaves the set that later passes scan.
  `CACHEDIR.TAG` and the other profiles of the target stay.
- **Whole targets, with `--evict-whole-target`.** `evict::whole_targets(targets, chosen)` returns
  the targets whose every profile dir the selection took; the pass then plans one
  `Action::RemoveTarget` for such a target instead of the per-profile `Remove` actions inside it,
  so `doc/`, `package/`, `tmp/` and `CACHEDIR.TAG` go with them — what `cargo-clean-all` and
  `kondo` do, and what evicting profile by profile leaves behind. Each `Whole` carries its
  profiles, so the re-check under the lock covers all of them: one profile busy or built since
  the inventory keeps the target dir, and the free profiles are still evicted one by one. Only
  `target/` goes; the project around it is never touched.
- **Always reported.** Every planned removal and its reason land in `PassReport::removals`, on a
  dry run too; the CLI prints them as `would remove` / `remove`.
- `evict` runs after `orphans` and before the lossless passes: no point compressing what is
  about to go.

Known limits: a busy profile is skipped, so a run may end above the cap; the cap is checked
against sizes from the inventory, taken before compress and dedupe shrink the rest; a profile
dir that disappears between the inventory and the lock (a concurrent `cargo clean`) fails the
run with the I/O error instead of being skipped. Thresholds are flags until the config (T10).

## Incremental pass (`src/incremental.rs`)

Lossy, so it runs only with `--lossy incremental --incremental-idle-days <N>`; the flags need
each other, as `evict`'s do.

- **What it drops.** `<profile>/incremental/`, rustc's incremental-compilation cache, in every
  profile dir whose last build is at least N days old. Cargo writes it for workspace members
  only, so no dependency has anything there to lose.
- **Measured cost: nothing, until the next edit.** The cache is not part of cargo's fingerprint.
  After the pass the build oracle reports zero stale units (`tests/incremental.rs`); the price
  is paid on the next change to a workspace member, which is then compiled non-incrementally
  once. That is why the pass is for profiles you are *not* working in, and why `--min-age` style
  floors do not apply to it: the age that matters is the profile's last build.
- **Selection is pure**, except for one `is_dir` check: a profile with no cache is never planned.
  A profile whose last build is unknown is never chosen, as in `evict`.
- **Re-checked under the lock**: the cache goes only if the engine holds that profile's lock and
  `inventory::last_built` still equals the inventory's reading. A build in between keeps it.
- **The engine removes, not the pass.** `Action::Remove` accepts a locked profile dir *or a dir
  inside one*, which is what makes this pass one selector instead of a second removal path.
  The profile dir itself stays, so its lock stays valid for the passes that follow.

Known limits: the whole cache of a profile goes or none of it — cargo's per-crate session dirs
are not read; a busy profile is skipped; `docs/research.md` measured `incremental = false` at
−5.8 GB on one target, but the pass's own yield across a machine is not benchmarked yet.

## Inventory and `status` (`src/inventory.rs`)

Read-only: takes no locks and changes nothing, so it is safe next to running builds.

- **Discovery.** Walk the roots without following symlinks; a dir whose `CACHEDIR.TAG` holds
  cargo's sentence is a target. A found target is not entered, so a target nested inside another
  is part of the outer one. Caches of other tools carry the same tag file and are ignored.
- **Sizes.** The whole target is scanned with the engine's inode model (`doc/`, `package/` and
  `tmp/` weigh too, not only profile dirs). `allocated_bytes` is `st_blocks` of every inode
  counted once — what `du` reports. `compressed_bytes` / `compressible_bytes` split the inodes by
  the compressed flag and the compress pass's size floor.
- **Family.** The nearest `.git` above the target. A directory is the common dir itself. A file
  (`gitdir: <path>`) is a worktree: the common dir is `<gitdir>/commondir`. Git's files are read
  directly, because the interesting case is the one where `git` refuses to answer.
- **Orphan.** The `.git` file points at a `gitdir` that no longer exists. The family is still
  known: the path has the form `<common dir>/worktrees/<name>`.
- **Dedupe estimate.** Per target, bytes of files of at least the dedupe size floor whose exact
  size also occurs in another target of the family. An upper bound: equal size is not equal
  content, and nothing is hashed for a report.
- **Last built.** Newest mtime among the top-level entries of the profile dirs.

`run` builds the same inventory and starts one engine run per family (a target without a family
runs alone), so locks are held only in targets that are compared with each other. Known limit:
equal files in unrelated projects are not shared.

## Advise command (`src/advise.rs`)

Read-only, and the only command that reads anything outside a target dir. Two lists:

- **Findings** come from files. `review(file, kind, doc, nightly)` is pure over a parsed
  `toml::Table`, so every check is a unit test over a literal document. A manifest contributes the
  `[profile.*]` checks (`debug` spelled any of the three ways cargo accepts, the `"*"` dependency
  override, `strip` for release, `split-debuginfo = "packed"`, `codegen-units = 1` in a dev
  profile); a config contributes `build.incremental` and, on a stable toolchain, the `[unstable]`
  table cargo ignores without a word; the cargo home's config is also asked for
  `cache.auto-clean-frequency`, which cargo has cleaned by since 1.88. A missing key is a finding
  as much as a wrong one — cargo's own default for `profile.dev.debug` is full debuginfo — so a
  finding names the file and the key it is *about*, which is not always a key the file has.
- **Notes** come from the inventory, because no single file explains them: what `incremental/`
  weighs under the roots, families whose targets could share a `[build] build-dir` (stable since
  1.91, at the price of serializing parallel builds on one lock), checkouts with no target dir
  that `seed` would fill, and orphaned worktrees for `--lossy orphans`.

The toolchain channel is only asked for (`rustc --version` in the project dir) when a config has
an `[unstable]` table, since that is the only check it decides; a rustc that cannot be run counts
as stable. Numbers quoted in the advice are the measured ones in `docs/research.md`.

Out of scope on purpose: `cargo-hakari` and `sccache` help with rebuild time, not with the size
of a live target, and nothing in a file says whether a workspace wants them — they stay in
`docs/research.md` rather than in the output.

## Doc pass (`src/doc.rs`)

Lossy, so it runs only with `--lossy doc`, and the smallest pass there is: `<target>/doc` is what
`cargo doc` writes from scratch and no build reads, which is why `cargo clean --doc` exists.

`doc/` sits beside the profile dirs rather than inside one, so the lock that guards it is the
target's own: the pass plans `Action::RemoveTarget { target, dir }` with `dir` the `doc/` dir, and
the engine applies it only while it holds a profile lock inside that target and nothing in the
target is busy. That is the same guard `orphans` and whole-target eviction use, which is why
`RemoveTarget` names the target and the dir separately instead of assuming they are the same.
The size comes from the inventory (`Target::doc_bytes`), since the engine itself scans only
profile dirs. The build oracle is untouched by it: after the pass cargo reports nothing stale.

## Toolchain report (`src/toolchains.rs`)

A toolchain upgrade does not clean up after itself: cargo compiles every unit again under new
hashes and never looks at what the old rustc produced. `cargo-sweep --installed` finds those by
parsing hashed file names; this reads cargo's own fingerprints instead —
`<profile>/.fingerprint/<unit>/*.json`, whose `rustc` field is cargo's hash of the compiler it
used. The unit is a directory and the compiler is a number cargo wrote, so no name is parsed.

The groups are sorted by their newest fingerprint, which makes the head the compiler in use and
everything after it stale. Bytes are an **estimate**, and say so in the field name
(`stale_bytes_estimate`): the profile dirs' size in the share of the units. An exact number needs
the unit-to-file map that only cargo's newer build-dir layout gives (roadmap `R1`), and a
fingerprint names no artifact.

This is why the task stops at a report: nothing can be deleted safely without that map, so
`status` prints a line per target and `advise` adds a note pointing at `cargo clean`. A target
built by one rustc — the ordinary case — reports nothing at all.

## Cargo home (`src/cargo_home.rs`)

The registry sources are the one big pile of compressible text outside the targets: every crate
cargo builds is unpacked there once and then only read. `--cargo-home` treats it as one more
group, with two differences from a target.

- **Only `compress` runs.** Nothing there is a build artifact: there is nothing to dedupe against,
  nothing stale to evict. The two dirs it touches are `registry/src` and `git/checkouts` — the
  extracted sources. `registry/cache` (the `.crate` archives) and `registry/index` are left alone;
  the archives are already compressed, and the index is cargo's own cache to invalidate.
- **One lock for the whole group, not one per dir.** Cargo does not write `.cargo-lock` files
  there; what it holds while it fetches or extracts is `<home>/.package-cache`. So
  `engine::run(..., Locks::Shared(&lock))` takes that one file lock and either all the dirs are
  ours or none are, which is also why a home cargo has never used (no `.package-cache`) is refused
  rather than locked into existence.

What decides whether cargo re-extracts a crate is `.cargo-ok` and the files beside it, and
compression changes neither the content nor the mtime of any of them — the test asserts that over
every file in a fake home, and the benchmark confirms it against a real one by rebuilding
afterwards. The flag takes an optional value: `--cargo-home` alone resolves `CARGO_HOME`, else
`$HOME/.cargo`. `status --cargo-home` reports the same dirs without touching them, and is opt-in
because measuring them costs a second full walk.

## CLI surface

```
cargo tare status [--json] [--cargo-home [DIR]] [ROOT]...  # inventory, families, potential
                                                          # savings; read-only
cargo tare run [--dry-run] [--lossy <PASS>]... [--index <FILE>] [<ROOT>]...
               [--config <FILE>] [--json]          # file: see below; json: the report as data
               [--cargo-home [DIR]]                # compress the registry sources too
               [--evict-idle-days <N>] [--evict-max-total-gib <N>]   # with --lossy evict
               [--evict-whole-target]                                # with --lossy evict
               [--incremental-idle-days <N>]        # with --lossy incremental
                                                    # --lossy orphans: no threshold
               [--pass <PASS>]... [--min-age <SECS>] [--min-size <BYTES>]  # benchmarks
cargo tare seed [--from <DIR>] [--dry-run] [--index <FILE>] [<DIR>]  # clone a sibling's target
cargo tare advise [--json] [ROOT]...  # what makes these targets bigger than they need to be
```

Config (`src/config.rs`): `$XDG_CONFIG_HOME/cargo-tare/config.toml`, else
`~/.config/cargo-tare/config.toml` — `roots`, `lossy`, `min-age`, `min-size`,
`[evict] idle-days / max-total-gib / whole-target`, `[incremental] idle-days`, `[family."<dir>"] skip`. Keys are
kebab-case and unknown ones are an error: a typo that silently does nothing is worse than a stop.
A flag always wins over the file, and a file named with `--config` must exist. Only `skip` is per
family, because the other thresholds are decided over everything under the roots at once.

Exit codes: `0` done, `1` failed, `2` a profile dir was left alone because a build held its lock.
A scheduled run needs that difference; anything else it wants is in `--json`, which prints the
groups, the busy dirs, the per-pass counts, every removal with its reason and every skip.

## Dependencies

Order of preference: std, then crates already in the shared Rust inventory, then new crates. Each
crate is wired by the task that first needs it and lands in `toolchain.md` in the same change.
The creator approved every crate in the table below; anything outside it still needs approval.

std already covers: inode identity and allocated size (`MetadataExt`: `dev`, `ino`, `nlink`,
`blocks`, `st_flags` for the compressed flag), file locks (`File::try_lock`, 1.89+), mtime restore
(`File::set_times`), atomic `rename`, `env::home_dir`, reading a worktree's `.git` file and
`commondir` for family grouping.

| Task | From the shared inventory | Why |
| --- | --- | --- |
| T3 | `tempfile`, `serde`, `serde_json`, `assert_cmd`, `predicates`, `trycmd` | Fixture in a temp dir; parse `cargo build --message-format=json` for the oracle; exit codes via `assert_cmd`, full CLI output via `trycmd` fixtures |
| T4 | `walkdir`, `rayon`, `serde_json`, `anyhow` | Tree walk, parallel `stat`, `status --json`, one error type for the binary |
| T5 | `rustix`, `thiserror`, `tracing`, `tracing-subscriber` | `clonefile` without hand-written `unsafe`; typed engine errors that name the path; `-v` logs |
| T7 | `sha2`, `rayon` | Content hashing across files |
| T10 | `toml`, `indicatif`, `owo-colors` | Config file, progress, coloured report |
| T11 | `divan` | Micro-benches for scan and hash; whole-build timings stay a script |
| T12 | `toml_edit` | Read `~/.cargo/config.toml` and print exact suggested edits |
| when it helps | `rstest`, `proptest`, `pretty_assertions`, `strum` | Per-pass test cases, plan invariants over random hardlink groups, pass names |
| release | `clap_complete`, `clap_mangen`, `cargo-dist`, `git-cliff`, `cargo-nextest` | Only when the tool is published |

Deliberately not used: `ignore` (its filters would hide git-ignored `target/`; `walkdir` + `rayon`
is enough), `nix` and `libc` (`rustix` covers the same calls safely; `libc` only if `rustix` lacks
one), `figment` (one config file), `directories` / `dirs` (the config path is fixed), `chrono`
(ages are `SystemTime` arithmetic), `heed` / `diesel` (the hash index starts as a flat file; `heed`
is the fallback if loading it shows up in T11), `insta` (CLI output goes through `trycmd`),
`notify` and `tar` (only for the watch-mode and park ideas in `ideas.md`).

`applesauce` (compression backend) was approved outside the inventory and joined it with T6.
Not in the inventory: `blake3` (approved, not
needed yet: hardware SHA-256 measured 1484 MB/s per core — the earlier slowness was perl `shasum`,
389 MB/s — so `sha2` + `rayon` across files comes first). Human-readable sizes and durations
(`bytesize`, `humantime`) are not in the inventory either; a small function each, and `min-age` in
the config is a number of hours.

## T2 spike results

Run inside a throwaway APFS image; scripts and raw output are in `docs/spike/`.

| Question | Result |
| --- | --- |
| Seed by recursive clone | 0.09 s, +108 KB for a 52 MB target; 13 of 15 units fresh, 2 members rebuilt; 0.7 s vs 3.0 s cold; mtime preservation irrelevant |
| Byte-identical artifacts between independently built worktrees | 41 of 52 files, 57% of bytes; differing: members, the proc-macro dylib, build-script binaries, 5 rlibs / 3 rmeta (absolute paths such as `OUT_DIR` are the suspected cause) |
| Clone-replace with restored mtime | −24.2 MB of −28.1 MB expected; all units fresh |
| Clone-replace without restored mtime | registry rlib: still fresh; member rlib: dependent rebuilt |
| Hardlink-group replacement (clone → `ln` + `rename` per path) | group keeps one inode and its link count; all units fresh; groups link `deps/` ↔ `incremental/` |
| `applesauce` on a target with hardlinks and clones | 52.3 → 18.2 MiB (−65%), 46 hardlinked files before and after, mtime + mode signature identical, all fresh, 0.68 s, re-run 0.16 s |
| Compressing a cloned target | image usage **+16 MB**: un-shares the clones |
| Seeding from a compressed target | +180 KB, clones stay compressed, 13 of 15 fresh |
| Re-cloning separately compressed files from a compressed canonical | −14.5 MB of −16 MB expected, still compressed, all fresh |
| Relink against compressed rlibs | works; files rewritten by the build lose compression, which justifies `min-age` |
| cargo lock | cargo blocks on a foreign `flock` of `target/debug/.cargo-lock` ("artifact directory") |
| Hash throughput | openssl SHA-256 1484 MB/s, perl shasum 389 MB/s, BLAKE2b 826 MB/s |

Open after T2: `applesauce` left a single 119 MB blob uncompressed with all three codecs
(reason not captured). T6 did not reproduce it: a 130 MiB file compresses through the library, so
size is not the limit (the backend's source refuses only files of 4 GiB and more — not tested
here). Every codec gave up on the blob in 0.13 s, which reads as a skip, not as a failed attempt;
the pass now keeps the backend's skip reasons and errors and `run` prints them.
Decompression cost at link time was not measurable on the small fixture — T11.

## Platform

v0.x is macOS / APFS only. The model (inodes, families, planner) is platform-neutral; a backend
supplies `clone`, `compress`, `is_compressed`.
