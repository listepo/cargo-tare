# Shrinking Cargo `target/` — research

Status: research only, nothing deleted or changed on the machine. Measured 2026-09-18,
macOS / APFS / Apple Silicon, stable cargo 1.97.1. Web facts were collected by cheap
sub-agents and spot-checked against crates.io / GitHub APIs and the Inside Rust blog.

## Correction (after a git-aware inventory, same day)

The claim below that every target dir was "built today" and that there were "no idle dirs" was
wrong — it came from an unverified sub-agent report. A scripted inventory found that **30 target
dirs, 113.9 GB of the ~158 GB, sit in `rtok` worktree checkouts that `git worktree list` no longer
registers**, last built 6–7 days earlier, with no build lock held. So removing whole idle /
orphaned targets is the single largest lever on this machine (~72%), ahead of compression and
dedupe, which apply to the remaining live targets. The in-target numbers (file classes,
compression ratios, content-hash dedupe) are unaffected. `cargo-clean-all --keep-days` would have
caught these dirs; nothing existing detects them as orphaned worktrees.

## TL;DR

- `cargo-cache` is **not** an alternative: it only cleans `~/.cargo/registry` and
  `~/.cargo/git` (618 MB here). It never touches `target/`. It is also unmaintained (last
  release 2022-09).
- The problem on this machine is **not stale artifacts**, it is **many live copies**:
  44 target dirs / ~158 GB under `~/GitHub`, 30 of them git worktrees of one repo (`rtok`),
  all built today. Age-based cleaners (`cargo-sweep --time`, `cargo-clean-all --keep-days`)
  find ~50 MB of 14 GB.
- Biggest lever measured: **transparent filesystem compression** (deps compress to ~20% of
  logical size with zstd-3). Second: **reflink (clonefile) dedupe** — 36% of bytes across four
  sampled target dirs are duplicate content. Third: a **shared `build-dir`** for worktrees of
  one repo (stable since cargo 1.91), which costs build-lock contention between parallel agents.
- No existing tool combines these. Existing pieces exist for each step, so the first version
  should be a thin orchestrator, not a new cleaner. A custom crate is justified only for the
  gaps listed in "Proposed tool".
- Build-time comparison was **not measured**: 3.6 GB free disk and other agents building in
  the same dirs. Plan is in "Experiments".

## Measured facts

| Fact | Value |
| --- | --- |
| Disk | 926 GiB, 100% used, 1.1–3.6 GiB free during the session |
| Cargo target dirs under `~/GitHub` | 44 dirs, ~158 GB |
| of which worktrees of `rtok` with a `target/` | 30 |
| Largest | `rtok/.claude/worktrees/graph-perf` 17.7 GB, `rtok-gate-sep` 13.9 GB, `rtok` 11–14.8 GB |
| Last built | every target dir: today → no idle dirs to drop by age |
| Files older than 7 d inside `rtok/target/debug` | ~50 MB |
| `~/.cargo/registry` | 618 MB (the only thing cargo-cache could clean) |
| `~/.rustup/toolchains` | 7.1 GB |

`rtok/target/debug/deps`, 10.65 GB logical (63k files):

| Class | Bytes | Share | zstd-3 ratio (files > 256 KB) |
| --- | --- | --- | --- |
| `.o` (57,754 files) | 4.79 GB | 43% | 5.4% |
| `.rlib` | 2.45 GB | 22% | 22.5% |
| `.rmeta` | 1.74 GB | 16% | 32.5% |
| test/bin executables (98) | 1.63 GB | 15% | 21.0% |
| `.dylib` + `.a` | 0.54 GB | 5% | not measured |
| `incremental/` (5.76 GB, separate dir) | — | — | 36% |

Content-hash dedupe (sha256, files > 256 KB in `debug/deps`):

| Pair | Duplicate bytes | Of |
| --- | --- | --- |
| `rtok-gate-sep` vs `rtok` | 3.07 GB | 11.02 GB (28%) |
| worktree `T35.3` vs `rtok` | 0.89 GB | 4.15 GB (21%) |
| worktree `graph-perf` vs `rtok` | 0.30 GB | 10.23 GB (3%) |
| duplicates inside `rtok` alone | 2.44 GB reclaimable | 7.4 GB (33%) |
| union of all four | **10.14 GB reclaimable** | 28.45 GB (36%) |

Other observations:

- 70,975 files in `rtok/target/debug` have link count > 1. rustc/cargo already hardlink
  between `incremental/`, `deps/` and the profile root. Any tool that rewrites files
  (compress, dedupe) must treat a hardlink group as one unit or it will *increase* usage.
- No file carries the APFS `compressed` flag today.
- `rtok` profiles are already tuned (`debug = "line-tables-only"`, deps `debug = false`).
  That lever is spent.
- `[unstable] no-embed-metadata = true` in `~/.cargo/config.toml` is ignored on stable.
- The crate `rtok` has 10+ incremental dirs with different hashes (~3 GB): profile / feature /
  test-target variants, some certainly orphaned. Age cannot tell which.

Data quality: absolute sizes moved 20–30% between runs because other agents were building.
An agent-reported "5.1 GB of stale incremental sessions" was rejected (it counted lock files).
Compression was measured with zstd-3 through a pipe; APFS uses LZFSE/LZVN per file with
per-file overhead, so real ratios will be worse than the table.

## Existing tools

Metadata from crates.io / GitHub API on 2026-09-18.

| Tool | Version / updated | Touches | Selection | Notes |
| --- | --- | --- | --- | --- |
| cargo-cache | 0.8.3 / 2022-09 | `~/.cargo` only | — | unmaintained; irrelevant to `target/` |
| cargo-trim | 0.16.0 / 2026-07 | `~/.cargo` only | orphan / old crates | active |
| cargo-sweep | 0.8.0 / 2025-10 | inside `target/` | `--time`, `--installed`, `--toolchains`, `--maxsize`, stamps | README says unmaintained; `build-dir` support open (#140); parses hashed file names |
| cargo-gc-bin | 0.1.7 / 2025-03 | inside `target/` | keeps what `cargo build` reports, deletes the rest | one project, needs a full build, no dry-run documented |
| cargo-clean-all | 0.6.5 / 2026-08 | whole `target/` dirs | `--keep-days`, `--keep-size`, recursive, `--dry-run` | active; right tool for idle projects |
| kondo, cargo-wipe, cargo-clean-recursive, cargo-cleaner | 2024–2026 | whole `target/` dirs | recursive scan | behaviour not re-verified from READMEs |
| cargo-apfs-compress | 0.1.2 / 2026-02 | inside `target/` | LZFSE, post-build, skips compressed files, locks profile dirs | 78 downloads, single author, no benchmarks |
| applesauce | — | any dir | APFS compression CLI | generic; not re-verified |
| fclones / jdupes | — | any dir | content dedupe, reflink on APFS | generic; mtime preservation must be verified |

Gaps no tool covers: size-capped LRU across projects, cross-project dedupe aware of cargo
hardlinks and mtimes, automatic compress-after-build, orphaned-worktree detection,
layout-independent operation.

## Cargo built-ins

| Feature | Status | Relevance |
| --- | --- | --- |
| `build.build-dir` / `CARGO_BUILD_BUILD_DIR` | stable since 1.91; templates `{workspace-root}`, `{cargo-cache-home}`, `{workspace-path-hash}` | intermediates in one place, final artifacts stay in `target/` |
| build-dir layout v2 (`-Zbuild-dir-new-layout`) | nightly; reported as landing in 1.100 (changelog not opened by me) | breaks tools that parse `deps/*-<hash>` |
| global cache GC (`cache.auto-clean-frequency`) | stable since 1.88 | `~/.cargo` only |
| GC for `target/` / build-dir | not implemented (rust-lang/cargo#5026) | the open niche |
| per-user shared artifact cache | not implemented (rust-lang/cargo#5931) | would make most of this unnecessary |
| `-Zembed-metadata=no` | nightly default since 2026-08; dev+incremental+debuginfo −4.7…−7.6%, release up to −33% | free once stable; nothing to do now |
| `-Zfine-grain-locking` | nightly | would remove the main cost of a shared build-dir |

## Methods compared

Size numbers are from this machine unless marked (pub) = published elsewhere.
Build-time column is qualitative or published; nothing was benchmarked here.

| Method | Size effect | Build-time effect | Fit here | Risk |
| --- | --- | --- | --- | --- |
| cargo-cache / cargo-trim | ≤ 0.6 GB | none | none | none |
| Age-based sweep inside active target | ~50 MB of 14 GB | none | none | low |
| Drop whole idle `target/` (cargo-clean-all) | 0 today (all active) | full rebuild of dropped project | useful later, as policy | low |
| Drop `target/` of removed / merged worktrees | unknown, likely large over time | none for live work | high | low |
| cargo-gc style (keep what the build reports) | unmeasured; candidates: duplicate units, ~3 GB of `rtok` incremental variants | needs a build; `cargo check` redone after | medium | medium: variants used by other commands get deleted |
| Profile tuning (`line-tables-only`, deps `debug=false`) | already applied; (pub) −27…−35% | slightly faster | spent | none |
| `incremental = false` | −5.8 GB per target (−40%) | (pub) local rebuilds 1.4–5× slower | poor for dev | none |
| `-Zembed-metadata=no` | (pub) −5…−8% dev | none | wait for stable | nightly only |
| Shared `target-dir` for everything | large | lock contention, feature thrash, binaries overwrite, one `cargo clean` wipes all | poor | high |
| Shared `build-dir` per repo (all worktrees) | dedupes third-party deps; workspace crates still per path; expected 20–35% by the dedupe numbers | new worktree builds deps 0×; parallel agents serialize on the build lock | good for size, bad for 5 parallel agents | medium; orphans accumulate, no GC |
| sccache | **+** size (cache plus targets) | (pub) CI 45→5 min; local warm +11%, cold −17% | speeds new worktrees only | low |
| cargo-hakari | none | (pub) fewer rebuilds in workspaces | unrelated to size | low |
| APFS compression post-build | deps to ~20–30% of logical (zstd-3 proxy; README claims < 50%) | small CPU on read; rewritten files lose compression → re-run after builds | **highest** | hardlink handling, lock vs running builds |
| Reflink dedupe across target dirs | 36% on sampled dirs; 3% for a diverged worktree | none (CoW) | high for sibling worktrees | must preserve mtime or cargo rebuilds; hardlink groups |
| tar+zstd of parked targets | ~−75% | unpack before use | only for parked worktrees | low |

Rough projection for the 158 GB: dedupe (−20…36%) then compression of what remains
(to ~25–35%) → **roughly 30–45 GB**. This is an estimate from four sampled dirs, not a measurement.

## Proposed tool (working name `target-slim`)

Write only what no existing piece does; operate on directories and file content so layout v2
does not matter. No parsing of `name-<hash>` file names.

1. Discover target dirs under configured roots (via `CACHEDIR.TAG` plus a cargo marker, since
   gradle / uv / huggingface caches also carry `CACHEDIR.TAG`).
2. Skip dirs with a held cargo build lock; take the lock while mutating.
3. Orphans: target dirs whose worktree is gone or whose branch is merged → report / delete.
4. Dedupe by content hash with `clonefile`, per hardlink group, restoring mtime and permissions.
5. APFS-compress what is left, skipping compressed files and respecting hardlink groups.
6. Optional global size cap: evict whole profile dirs of least-recently-built projects.
7. `--dry-run` everywhere, default on; report bytes per step.

Candidate dependencies (need creator approval, none added yet): `applesauce` (compression
library), `reflink-copy` (clonefile), `walkdir` / `ignore`, `blake3`.

macOS / APFS only in v1. On Linux the same idea maps to btrfs / XFS reflinks plus fs-level zstd.

## Recommendation

1. Free space first (creator's decision; nothing was deleted): worktree targets are rebuildable.
2. Phase 0, no code: on one sibling pair run `cargo-apfs-compress` (or `applesauce`) and a
   reflink dedupe (`fclones dedupe`) and measure real bytes, rebuild behaviour and hardlink safety.
3. Phase 1, if phase 0 holds: a `just` recipe chaining existing tools after builds.
4. Phase 2, only if the chain shows the gaps matter (hardlinks, mtime, locks, orphan worktrees,
   size cap): write `target-slim` as above.
5. In parallel, try a repo-level `build-dir` for `rtok` worktrees in one worktree pair and measure
   lock waiting with parallel agents; adopt only if waiting is acceptable.

## Experiments still needed (blocked on disk space)

- Clean and incremental build times: baseline vs compressed target vs deduped target vs shared
  `build-dir` vs sccache, same commit, `hyperfine`, 3 runs each.
- Does cargo consider a deduped / compressed target fresh (`cargo build` → 0 rebuilt units)?
- Do APFS compression and clones compose, and what happens to hardlink groups?
- Real LZFSE ratio including small files; CPU cost at link time.
- Why `graph-perf` shares only 3% with `rtok` (toolchain, flags, lockfile?).

## Sources

- https://blog.rust-lang.org/inside-rust/2026/08/18/reducing-target-dir-size-on-nightly/
- https://kobzol.github.io/rust/rustc/2025/06/02/reduce-cargo-target-dir-size-with-z-no-embed-metadata.html
- https://blog.rust-lang.org/2026/03/13/call-for-testing-build-dir-layout-v2
- https://doc.rust-lang.org/cargo/reference/config.html#buildbuild-dir
- https://doc.rust-lang.org/nightly/cargo/reference/unstable.html
- https://doc.rust-lang.org/nightly/cargo/CHANGELOG.html
- https://github.com/rust-lang/cargo/issues/5026 , https://github.com/rust-lang/cargo/issues/5931
- https://github.com/holmgr/cargo-sweep , https://github.com/waynexia/cargo-gc
- https://github.com/dnlmlr/cargo-clean-all , https://github.com/matthiaskrgr/cargo-cache
- https://github.com/bgw/cargo-apfs-compress , https://github.com/Dr-Emann/applesauce
- https://neosmart.net/blog/benchmarking-rust-compilation-speedups-and-slowdowns-from-sccache-and-zthreads/
- https://jacobdeichert.ca/blog/reducing-rust-incremental-compilation-times-on-macos-by-70-percent/
