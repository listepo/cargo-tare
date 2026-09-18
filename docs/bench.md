# Benchmarks

What the passes cost and what they win, measured on a copy of a real workspace. The numbers
below are one run of `scripts/bench.sh`; a second run with the same script produced the same
sizes and the same freshness result, and is quoted where the two disagree. The raw
`key<TAB>value` lines and the hyperfine JSON stay in the work dir the script prints at the end.

## How to reproduce

```
scripts/bench.sh ~/GitHub/listepo/apps/ketch          # RUNS=5 WITH_SCCACHE=1 by default
```

The script shallow-clones the workspace into a temp dir and adds one git worktree of that clone,
so the two checkouts form a family and dedupe has siblings to compare. It builds offline after a
single `cargo fetch`. **The workspace's own `target/` is never touched** — the tool only ever
sees the copies. Every measurement uses `--min-age 0`, because everything just built is younger
than the default age floor.

Per stage it records the tool's wall clock, target size before and after (`du -sk`), free space
on the volume before and after (`df -k`), how many units cargo rebuilds afterwards, and the mean
of `RUNS` incremental builds after touching the top-level crate (`hyperfine`, 3 warmups).

`du` counts every block a file is charged with, so it sees compression but **not** copy-on-write
sharing. For dedupe, only the free-space delta is the truth.

## The workspace

`apps/ketch`: 587 crates in the lock file, two checkouts of one family. Apple Silicon, APFS,
stable Rust 1.98, nothing else running.

| | checkout `a` | checkout `b` |
| --- | --- | --- |
| clean build | 61.6 s | 73.4 s |
| target after that build | 1.77 GiB | 1.77 GiB |

## What the passes win

Sizes are the two targets together, taken immediately before and after each pass.

| Pass | `du` before | `du` after | `du` delta | Free space delta |
| --- | --- | --- | --- | --- |
| compress | 3.63 GiB | 1.32 GiB | **−2.31 GiB (−63.7%)** | +1.0 GiB |
| dedupe (after compress) | 1.48 GiB | 1.48 GiB | 0 | **+407 MiB** |

Read the two columns differently, as above: compression shows up in `du`, sharing does not.
Together the passes take 3.63 GiB of freshly built targets down to about 0.93 GiB of blocks
actually on disk — a **74%** cut, on targets an age-based cleaner would not touch at all because
every file in them is minutes old.

The dry runs, taken on the pristine targets before anything was rewritten, predicted 3.61 GiB
compressible and 1.08 GiB duplicated. Dedupe then runs on already compressed files, which is why
it recovers 407 MiB rather than the full gigabyte.

## What the passes cost

| | wall clock |
| --- | --- |
| compress, both targets (3.6 GiB) | 81.7 s |
| dedupe, both targets | 12.4 s |
| both again, right afterwards | 2.7 s |

Incremental build after touching the top-level crate, mean of 5 runs:

| | mean | range |
| --- | --- | --- |
| baseline | 3.70 s | 2.39 – 5.10 s |
| after compress | 4.96 s | 2.78 – 10.17 s |
| after compress + dedupe | 3.53 s | 2.77 – 5.14 s |

The means are inside each other's spread, so the honest reading is: **no measurable slowdown**.
The one real effect is the first build right after a pass — 10.17 s, then 2.8 s for the rest of
the runs — because the pass rewrote every file and the page cache is cold. The first run of the
whole benchmark, with a single warmup, showed the same picture with much more noise (baseline
28 s falling to 5 s over five runs), which is why the script now warms up three times.

## Does anything get rebuilt afterwards?

No. After each pass, `cargo build --message-format=json` reported **0** units not fresh, in both
runs, on all 587 crates. That is the point of the design: mtimes are preserved, hardlink groups
stay groups, and cargo cannot tell that the bytes moved.

## A second run is still needed

Right after both passes, with nothing rebuilt in between, running the tool again still applied
**46** more actions (2.7 s). Dedupe's clones are new files that compress had never seen, so one
pipeline run does not reach a fixed point. Nothing is lost by it — the next scheduled run picks
them up — but a `run` that loops until it stops finding work would finish the job in one go.

## sccache, for comparison

| | clean build | target | cache |
| --- | --- | --- | --- |
| plain | 61.6 s | 1.77 GiB | — |
| `RUSTC_WRAPPER=sccache`, cold cache | 83.5 s | 1.46 GiB | 304 MiB |
| `RUSTC_WRAPPER=sccache`, warm cache | 32.3 s | 1.46 GiB | 304 MiB |

sccache answers a different question: it makes a *rebuild from scratch* about twice as fast, at
the price of a slower first build and a 304 MiB cache of its own. It does not shrink a live
target — its targets are smaller here only because a wrapper turns cargo's incremental
compilation off. The two are complementary, and nothing in `cargo-tare` conflicts with it.

## What the defaults are worth

- `min-size` 8 KiB for compress, 4 KiB for dedupe: a file smaller than a block cannot win a
  block. These numbers were not swept on this workspace — compression's win is so large that the
  floor only decides how many tiny files are walked for nothing.
- `min-age` 1 h: not a size decision at all. It keeps the tool away from files a build may still
  be writing, and this benchmark had to set it to 0 to measure anything. Leave it alone unless
  you are benchmarking.

## Not measured

- **Seeded worktree** — `cargo tare seed` does not exist yet (T8).
- **Shared `build-dir`** — cargo's `build.build-dir` is nightly-only (`-Z build-dir`); this
  machine builds on stable, where the key is ignored. T12's `advise` reports exactly that.
- **One workspace, one machine.** Every number above is `apps/ketch` on one Apple Silicon laptop.
  The compression ratio depends on what the crates emit; the build-time result should not.
