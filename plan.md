# dunnage

https://github.com/listepo/dunnage

A tool (a CLI, and a daemon to come) that shrinks live `target/` directories without slowing
builds: transparent filesystem compression, copy-on-write dedupe across targets, clone-seeding
of new worktrees, and opt-in removal of orphaned or idle targets — planned together so the
approaches reinforce each other. Called `cargo-tare` until T42.
Design in `DESIGN.md`, measurements in `docs/research.md`.

| # | Status | Priority | Complexity | Readiness | Agent |
| --- | --- | --- | --- | --- | --- |
| T24 | todo | P1 | 3 | 0% | |
| T21 | todo | P2 | 5 | 0% | |
| T30 | todo | P2 | 4 | 0% | |
| T31 | todo | P2 | 4 | 0% | |
| T32 | todo | P2 | 4 | 0% | |
| T33 | todo | P2 | 3 | 0% | |
| T34 | todo | P2 | 4 | 0% | |
| T37 | todo | P2 | 3 | 0% | |
| T38 | todo | P2 | 3 | 0% | |
| T39 | todo | P3 | 2 | 0% | |
| T40 | todo | P3 | 3 | 0% | |
| T35 | todo | P3 | 2 | 0% | |

Blockers, take these first. **T24** blocks T21: nothing on Windows can be tested without it.
**T33** blocks T35. T35 is the
lowest priority in the plan by the creator's word: take it only when nothing else is free.

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

### T30. Swift: SwiftPM `.build/` and Xcode DerivedData

The most promising target after cargo: APFS is where the tool is strongest, DerivedData runs to
tens of GB, and the only known cure is deleting it. SwiftPM first — `.build/` sits in the
package like `target/` does, and SwiftPM refuses a second instance on it, so there is a lock to
find. DerivedData second: `info.plist` records `WorkspacePath`, which makes `orphans` and
`evict` direct; it needs T29. Spike: confirm the lock file and the call, the plist keys on the
current Xcode, the compress and dedupe yield, and an oracle (`swift build` twice, the second
compiles nothing). Reading a plist may need a crate or `plutil`; a new dependency is the
creator's call at claim time. `seed` does not apply — the dir name is a hash of the path.

### T31. .NET: `bin/` and `obj/`

The highest dedupe yield in the study: every project's `bin/` holds its own copy of every
transitive NuGet assembly, byte-identical to the one in `~/.nuget/packages`. Discovery by
`obj/project.assets.json` next to a project file, `artifacts/` with `UseArtifactsOutput`.
**Clones only, never the hardlink fallback** — MSBuild's `Copy` overwrites in place, which is
how its own hardlink option corrupts the NuGet cache (dotnet/msbuild#8273); `--link-artifacts`
must be refused here, not merely off. No lock, and on Windows worker nodes keep files open:
needs T29. Oracle: `dotnet build` twice, the second reports every target skipped — that also
settles whether `CoreCompileInputs.cache` survives a same-content, same-mtime replacement.
macOS and Linux first; Windows needs ReFS and therefore T21. `seed` does not apply.

### T32. C and C++: CMake, Meson and Ninja build dirs

`compress` is the strong case — uncompressed DWARF in objects and static libraries; cargo's own
`.o` files went to ~5%. `orphans` is easier than for cargo: `CMakeCache.txt` records
`CMAKE_HOME_DIRECTORY`, so "the source is gone" is one `stat`. `dedupe` is expected to find
little (absolute paths in objects) and is measured, not assumed; never hardlinks. `seed` does
not apply — the build dir is full of absolute paths — and `advise` recommends `ccache` with
`file_clone = true` for that job instead. No lock: needs T29, and the spike first settles
whether current Ninja takes one. Plain Make has no marker and builds in the source tree: out of
scope. Oracle: `ninja -n` after a pass plans nothing.

### T33. Compress an immutable content-addressed store

One mode instead of five adapters: `~/.cabal/store`, the Zig caches, dune's shared cache and
the like hold immutable files under hashed names. `dedupe` finds nothing there by
construction, and `compress` is safe without a lock for the same reason — a name never gets
different bytes — with `min-age` keeping the pass off what is being written. The user names the
dir; nothing is discovered or guessed, and stores that compress themselves (ccache, sccache)
are refused by their marker files. Done: mtime, mode and content of every file unchanged, the
owning tool's own verification green on a fixture store, numbers in `docs/bench.md`.

### T34. Daemon mode: `dunnage daemon`

Decided by the creator: the tool runs as a CLI and as a daemon, sharing as much code as can be
shared. Design in `docs/architecture.md`, "Process model". The daemon is the same binary in the
foreground, kept alive by the service manager; it never forks itself. Everything it *does* is a
`Session` call (T36) with the `Request` a CLI run would build from the same config. What is its
own: triggers (a slow timer that re-runs discovery, a fast one over known units, and a
filesystem watcher on the top level of known units as one more trigger), per-unit due times
(`last write + min-age`, so a unit is visited once per build, when it has gone cold), and a
state file that `daemon status` reads. There is no IPC: CLI and daemon coordinate through the
run lock and files.

`dunnage daemon install | remove | status` writes and removes the launchd agent or systemd
user unit — low CPU and I/O priority set there, not in code — and replaces the hand-written
plist in `README.md`. A Windows service waits for T21.

Hard requirements: a build never waits for the daemon (`lock_budget` set low by default; the
test starts a build during a daemon pass and bounds how long it blocks); lossy passes run only
when the config enables them; the daemon adds no code path that mutates a build dir. The
watcher crate (`notify` is the candidate) is a new dependency and the creator's call at claim
time; the timers alone are a complete first version. Logging is the observer's events on
stderr, which launchd and journald already collect — no logging crate.

### T37. Monorepo: `seed` every position

`seed::choose` already looks "at the same place inside the sibling checkout", but for one dir
with the hardcoded name `target`. A monorepo checkout has many build dirs. `seed` in a checkout
root seeds every position: for each build dir of the sibling checkouts whose adapter allows
seeding, if the owner's project exists in the new checkout and the position is empty, clone it
from the sibling where *that position* was built most recently — no single worktree is the
newest everywhere. Positions whose project is absent on this branch are skipped. `seed <dir>`
keeps today's meaning. `dunnage worktree add` (T26) gets it for free. Uses T28's owner and
position. Done: on the monorepo fixture, two workspaces are seeded from two different siblings
and the project that exists on one branch only is left alone; the oracle is green for both.

### T38. Monorepo: `orphans` for a project that is gone

Today an orphan is a worktree whose git record is gone. In a monorepo the common orphan is
smaller: a project deleted, renamed or absent on this branch, whose build dir stays behind in a
live checkout. A second reason for the same lossy pass, from T28's owner: the owner's manifest
no longer exists. A branch switch produces the same picture as a deletion, so this reason
removes only units idle for longer than a threshold (`evict`'s `idle-days`, or one of its own)
and is otherwise only reported, by `status` and `advise` too. The *checkout gone* reason now
also covers build dirs outside the checkout (`build.build-dir`), which have no family today.
Done: fixture tests for both reasons, for "reported, not removed" without a threshold, and for
an out-of-tree build dir landing in its owner's family.

### T39. Monorepo: grouped `status` and `skip-paths`

One line per target stops being a report at a few dozen build dirs. `status` groups family →
checkout → subtotal per ecosystem, lists the largest build dirs up to a limit and the rest with
`--all`; `--json` stays flat and gains `ecosystem`, `checkout`, `position` and `guard` per build
dir, existing keys unchanged. Config gains `skip-paths` per family — positions as prefixes, so
no glob crate — and a per-family `ecosystems` list narrower than the global one.
Done: `tests/cmd` snapshots for a fixture with many build dirs; a skipped position is neither
reported as work nor touched; `docs/usage.md` documents both.

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
