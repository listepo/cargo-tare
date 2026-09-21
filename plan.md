# dunnage

https://github.com/listepo/dunnage

A tool (a CLI and a daemon) that shrinks live `target/` directories without slowing
builds: transparent filesystem compression, copy-on-write dedupe across targets, clone-seeding
of new worktrees, and opt-in removal of orphaned or idle targets — planned together so the
approaches reinforce each other. Called `cargo-tare` until T42.
Design in `DESIGN.md`, measurements in `docs/research.md`.

| # | Status | Priority | Complexity | Readiness | Agent |
| --- | --- | --- | --- | --- | --- |
| T24 | todo | P1 | 3 | 0% | |
| T21 | todo | P2 | 5 | 0% | |
| T30.1 | todo | P2 | 3 | 0% | |
| T32.1 | todo | P2 | 2 | 0% | |
| T38.1 | todo | P2 | 3 | 0% | |
| T40 | todo | P3 | 3 | 0% | |
| T35 | todo | P3 | 2 | 0% | |

Blockers, take these first. **T24** blocks T21: nothing on Windows can be tested without it.
T35 is the lowest priority in the plan by the creator's word: take it only when nothing else is free.

Decisions the plan is built on, all the creator's: the tool runs as a CLI **and** as a daemon
with as much shared code as possible, and must stay embeddable as a library in a build system —
not built now, R8 in `roadmap.md` (`docs/architecture.md`, "Process model"); Windows is tested
in a local VM, not in CI (T24); publishing waits (R7).

Where the tasks came from: `ideas.md` read against `docs/usage.md` (what a user trips over
today), `docs/ecosystems.md` (which build systems the engine fits — a desk study, so each
ecosystem task starts with a spike, and a spike that says "not worth it" closes the task with
that finding in `docs/research.md`) and `docs/architecture.md` (the monorepo tasks T37–T40).

### T24. A place where the Windows tests run

T21's first open point, made a task because everything else in T21 waits for it: `check-cross`
type-checks the Windows backend and nothing has ever run there.

Decided by the creator: a **local Windows VM**, not CI — the same choice as the lima VM for T20.
On Apple Silicon the guest is Windows on ARM, so `aarch64-pc-windows-msvc` joins `check-cross`
and is the target the suite actually runs on. Two volumes inside the guest: NTFS (the system
drive will do) and a ReFS Dev Drive made from a VHDX, with `TEMP` / `TMP` pointed at the volume
under test, the way `TMPDIR` picked btrfs or ext4 in the Linux VM. The VM software and how the
repository gets into the guest are settled with the creator when the task is claimed; nothing is
installed on the creator's machine without asking.

Done: the existing suite runs on NTFS in the VM and the result is recorded; the one contract
test known to fail there (`clone_file` is `fs::copy` where `caps` says no clones — T21's
analysis, point 2) is fixed by returning `Unsupported`; `AGENTS.md` says how to bring the VM up
and run the suite on either volume. No FFI in this task.

### T21. Windows: NTFS compression and ReFS block cloning

Compression: NTFS has per-file transparent compression through `FSCTL_SET_COMPRESSION`, and the
allocated size to measure it with comes from `GetCompressedFileSize`. Dedupe: ReFS has block
cloning (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`); NTFS has no copy-on-write at all, so dedupe there is
T22's hardlink fallback, which now exists and needs only `caps` to answer honestly there. File identity is `GetFileInformationByHandle`'s volume serial plus file index,
and the build lock stays `File::try_lock`, which is already cross-platform.

`windows-sys` is approved by the creator for this task; it lands in `toolchain.md` and
`rust.md` in the same change that wires it. Done: the pass suite
runs on ReFS, NTFS reports no block sharing and falls back to T22 instead of failing, and paths
with drive letters and `\\?\` prefixes are covered by tests.

#### Readiness analysis (not an execution plan; nobody has claimed the task)

State: `just check` is green (128 tests run on macOS) and both cross targets compile, but no test has ever
*run* on Windows — `check-cross` only type-checks. Open points, in the order they bite:

1. **Where the tests run — the real blocker.** Decided by the creator: a local Windows VM, not
   CI; it is T24. On Apple Silicon that is `aarch64-pc-windows-msvc`, which `check-cross` does
   not cover today.
2. **A contract test fails on Windows today.** `sys::windows::clone_file` is `fs::copy`, and
   `a_clone_holds_the_bytes_of_its_source_or_refuses_to_pretend` demands an error where
   `caps().clone` is false. `seed` already falls back to `fs::copy` on its own, so the fix is to
   return `Unsupported` — but it shows the suite needs a first run there before any FFI.
3. **Identity comes first.** `file_id` is a path hash and `nlink` is always 1, so after T22's
   hardlink fallback links two files, the next scan sees two unrelated files and plans the same
   link again, and `compress` would replace one name of a group and break it. Real identity
   (`GetFileInformationByHandle`) must land before `caps` answers anything but `NONE`.
4. **The `sys` signatures change.** `nlink(&Metadata)` and `allocated(&Metadata)` cannot be
   answered from `Metadata` on Windows (std's by-handle accessors are unstable); both need the
   path, on all three backends. `GetCompressedFileSizeW` takes a path, so `allocated` costs no
   handle, and `ALLOCATED_SHOWS_COMPRESSION` becomes `true` on NTFS.
5. **ReFS file ids are 128-bit.** The 64-bit index from `GetFileInformationByHandle` is not
   guaranteed unique on ReFS; `FILE_ID_INFO` is. `Stamp` holds `(u64, u64)`, so either it widens
   or the id is folded — a decision for the card, since the hash index format depends on it.
6. **NTFS and ReFS never overlap.** NTFS compresses and cannot clone; ReFS clones and has no
   per-file `FSCTL_SET_COMPRESSION`. The fused dedupe + compress path therefore never runs on
   Windows, and each half needs its own oracle test. Whether NTFS should use LZNT1 at all or
   WOF / LZX is in `ideas.md`.
7. **`FSCTL_DUPLICATE_EXTENTS_TO_FILE` details.** Destination must be sized first, ranges are
   cluster-aligned except at end of file, one call moves at most 4 GiB, both files on one
   volume with matching sparse and integrity state. It must fail on NTFS, never copy.
8. **The probe.** `probing_in` and `probe_path` are `cfg(not(windows))`. Restoring a directory's
   mtime on Windows needs a handle opened with `FILE_FLAG_BACKUP_SEMANTICS`; without that, the
   probe makes idle profiles look freshly built (the bug T20 already found once on btrfs).
9. **`rename` over an open file.** Another process holding the destination without
   `FILE_SHARE_DELETE` (an editor, antivirus, a running test binary) fails the `rename` with a
   sharing violation. The engine must count that as a skipped group, not a failed run.
10. **Paths.** `canonicalize` returns `\\?\C:\…` while git prints `C:/…`; family keys, the
    `[family."…"]` config key and `seed`'s sibling search must compare equal. Target dirs also
    routinely exceed `MAX_PATH`.
11. **Bookkeeping.** `windows-sys` under `[target.'cfg(windows)'.dependencies]` with the
    `Win32_Foundation`, `Win32_Storage_FileSystem`, `Win32_System_IO` and `Win32_System_Ioctl`
    features; `toolchain.md`, `rust.md`, the README platform table, the `DESIGN.md` platform
    section, and Windows numbers in `docs/bench.md`. This is the first hand-written `unsafe` in
    the crate (Linux avoided it through `rustix`), so each FFI call wants a safe wrapper with
    its invariants written down.

Suggested split if the creator wants it smaller: (a) test environment + item 2 — now T24,
(b) identity and the signature change, (c) NTFS compression, (d) ReFS cloning, (e) paths and
docs.

### T30.1. Swift: Xcode DerivedData

Split off from T30. `~/Library/Developer/Xcode/DerivedData/<name>-<hash>/`, with `info.plist`
recording `WorkspacePath`, which makes `orphans` and `evict` direct. No lock: needs the quiet
tier (T29), with `xcodebuild`, `XCBBuildService` and `SWBBuildService` as the tools. `plutil`
reads the plist without a new dependency. Needs a way to produce a DerivedData dir for tests
without writing into the real `~/Library` (`xcodebuild -derivedDataPath` in a temp dir is the
candidate; whether it writes `info.plist` there is the first thing to check). Oracle: a second
`xcodebuild` compiles nothing.

### T32.1. C and C++: Ninja and Meson

Split off from T32, which handles CMake build dirs and was verified with the Makefiles
generator only, because `ninja` and `meson` are not installed here. Settle whether current
Ninja takes a lock on the build dir, claim Meson build dirs (`meson-private/`, whose
`coredata.dat` records the source dir), and add the oracle `ninja -n` plans nothing after a
pass. Needs the creator's approval to install `ninja` and `meson` (brew or mise).

### T38.1. Monorepo: an owner for a build dir outside its checkout

Split off from T38. A cargo target moved out of the checkout by `build.build-dir` (or
`CARGO_TARGET_DIR`) has no family: `inventory::git_link` walks up from the target, not from the
project, and the build dir records no path back to the workspace that built it. Such a dir gets
no dedupe partner, no `seed` source and no *checkout gone* orphan status. Done: an out-of-tree
build dir lands in its owner's family, and `orphans` removes it when that owner's checkout is
gone, with a fixture test for both.

**Question for the creator before this starts:** where does the owner come from? Options:
(a) read the absolute source paths in the profile's dep-info `.d` files — present in every
build, but it is parsing cargo's output, a heuristic; (b) a record dunnage writes itself when
`seed`/`worktree add`/the daemon sees a build dir being used from a workspace — exact, but only
for dirs it has seen; (c) configuration: `[owners]` mapping build dirs to workspaces.

### T40. Known build dirs: a persisted inventory

Every run walks the roots to find build dirs, and in a monorepo the cost of that walk is the
source tree, not the build dirs. The daemon holds the list in memory; the CLI starts from
nothing each time. Persist what discovery found next to the hash index — build dir, adapter,
owner, marker stamp — re-validate entries by their markers, and walk only on a slow cadence,
on `--rediscover`, or when a root's own mtime says something moved. Measure first: the task
starts with a walk benchmark on a large checkout and closes with "not needed" if discovery is
already a small share of a settled re-run.

### T35. Go: `GOCACHE` and `GOMODCACHE`

Lowest priority in the plan. Go has no per-project build dir: `GOCACHE` is one content-addressed
store the `go` command trims on its own, so `dedupe`, `seed`, `orphans` and `evict` have nothing
to do (`docs/ecosystems.md`). What is left is `compress`, through T33's mode: `GOCACHE` found by
`go env GOCACHE`, and `GOMODCACHE` as the counterpart of `--cargo-home` — extracted sources,
which went down 69% for cargo. `GOMODCACHE` dirs are read-only on purpose; lifting and restoring
directory modes is the risk the spike has to price, and "leave `GOMODCACHE` alone" is an
acceptable outcome. Oracle: `go build ./...` after the pass reports every package cached
(`go build -x` runs no compile step) and `go mod verify` is green.
