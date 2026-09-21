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
| T36 | todo | P1 | 4 | 0% | |
| T21 | todo | P2 | 5 | 0% | |
| T25 | todo | P2 | 2 | 0% | |
| T26 | todo | P2 | 2 | 0% | |
| T28 | todo | P2 | 4 | 0% | |
| T29 | todo | P2 | 4 | 0% | |
| T30 | todo | P2 | 4 | 0% | |
| T31 | todo | P2 | 4 | 0% | |
| T32 | todo | P2 | 4 | 0% | |
| T33 | todo | P2 | 3 | 0% | |
| T34 | todo | P2 | 4 | 0% | |
| T37 | todo | P2 | 3 | 0% | |
| T38 | todo | P2 | 3 | 0% | |
| T39 | todo | P3 | 2 | 0% | |
| T40 | todo | P3 | 3 | 0% | |
| T41 | todo | P3 | 2 | 0% | |
| T35 | todo | P3 | 2 | 0% | |

Blockers, take these first. **T24** blocks T21: nothing on Windows can be tested without it.
**T36** blocks T25, T26, T28 and T34: the run has to live in the library before anything new is
added to it, or the daemon shares nothing. **T28** blocks T29–T33, T35 and T37–T40: every
adapter and every monorepo behavior sits on its boundary. **T29** blocks T31, T32, T33 and the
Xcode half of T30, which have no build lock to take. **T33** blocks T35. T41 is free. T35 is the
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

### T36. Library boundary: one `Session` under every front end

The blocker of everything the creator decided about how the tool runs: CLI and daemon share
the code, and embedding in a build system stays possible. Today the library holds the passes
and the engine but the *run* lives in the binary — `src/main.rs` is 806 lines, and `fn run`
alone (468–714) selects passes, groups families, picks what `evict` and `orphans` take, runs the
cargo home, loads and saves the hash index and decides the exit code; `status`, `advise` and
`seed_into` assemble their results there too. A daemon could share none of that.

Move it behind `Session` as sketched in `docs/architecture.md`, "Process model": `open`,
`inventory`, `advise`, `plan`, `apply`, `seed`; a `Request` both front ends build; a `Control`
with an `Observer` for progress and notes, a `stop` flag checked between groups of actions, and
a `lock_budget` after which the engine releases a unit's build lock and returns to it later.
The session takes the tool's own run lock (one file next to the hash index, `try_lock`, exit
code 2 for the CLI when held). The library stops printing, exiting and reading the environment
or config files on its own — `Settings` carries the paths, the resolving helpers stay as
functions a front end calls — and returns a typed `Error`; `anyhow` and `clap` move behind a
default `cli` feature that the `[[bin]]` requires. No workspace and no second crate.

No new behavior. Done: `main.rs` is parsing, printing and the exit code; every `tests/cmd`
snapshot and `tests/cli.rs` case is unchanged; `cargo check --lib --no-default-features` is part
of `just check`; a test drives a whole run through `Session` with no binary involved; two
sessions applying at once are serialized by the run lock; a `stop` raised mid-run leaves every
file either old or new; `DESIGN.md` gains the session next to "Engine".

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

### T25. `run` until nothing is left to do

`docs/usage.md` has to tell users that a second run still finds work: clones made by `dedupe`
are files `compress` never saw (46 actions on the benchmark workspace, `docs/bench.md`).
`DESIGN.md` already names the cure — repeat until a round plans nothing. The locks are held
once for all rounds, the report sums the rounds, `--dry-run` stays one round because it changes
nothing a second round could see. Done: a test where one `run` leaves a second `run --dry-run`
with an empty plan, the oracle green, and the troubleshooting entry gone from `docs/usage.md`.

Lands on the session (T36) as `Request::until_settled`, so the daemon settles a unit in one
visit exactly as the CLI does.

### T26. `dunnage worktree add`

The recipe in `docs/usage.md` is two commands — `git worktree add`, then `seed` — and the second
is the one people forget, which is exactly how a cold first build happens. One command that runs
`git worktree add` with the arguments it was given and seeds the new checkout from the family.
If git fails, nothing is seeded; if seeding fails, the worktree stays and the error says so.
Done: a test on the fixture repository creates a worktree whose first build reports third-party
units fresh; `--dry-run` passes through to `seed` only.

The git call and the seeding are one `Session` operation (T36); the CLI only parses and prints.

### T28. Adapter boundary: what is cargo and what is not

`docs/ecosystems.md`, first table: the engine, the inode model, the hash index, `src/sys/` and
the two lossless passes know nothing about cargo; discovery (`CACHEDIR.TAG`), the unit of work
(a profile dir), the lock (`.cargo-lock`), "last built", what `seed` leaves behind and the
cargo-only passes do. Put the second list behind one trait answering the six questions of that
document — discover, lock, freshness-relevant volatile paths, owner, last use, and the oracle in
tests — with cargo as its only implementation. No crate split until a second binary needs one,
no new flag, no behavior change: the existing tests and the `--help` snapshots are the
proof. The card of the first non-cargo adapter decides how an ecosystem is selected on the
command line; this one only makes room for it.

The shape is worked out in `docs/architecture.md`: the `Ecosystem` trait and its `Guard` /
`Policy` answers, `src/eco/` as the twin of `src/sys/`, one shared discovery walk where the
outermost claim wins (a CMake dir inside a cargo target is nobody else's), and family and
position computed from the build dir's *owner* rather than from where it sits — which is what
gives an out-of-tree `build-dir` a family at all. Part of done: the monorepo fixture described
there, with the assertions that already hold.

Sits on T36: discovery and the adapters are reached through the session, and `Guard::Held` is
reserved in the enum for an embedding caller (R8) without being implemented.

### T29. A safety tier for build systems without a build lock

Cargo holds one advisory lock for the whole build; Ninja, Make, MSBuild and Xcode hold nothing
an outsider can test. Without a lock the engine's re-check of `(size, mtime)` before each
`rename` narrows the race and does not close it. Build the weaker tier and name it as such in
every report: a larger default `min-age`, a sharing violation or a busy file counted as "busy"
(exit code 2) instead of a failure, a check for the build tool's running processes under the
dir, and a refusal of lossy passes when any of those says "maybe". Two runs of the tool itself —
the daemon and a manual one — are kept apart by the session's run lock (T36), which this tier
relies on. Done: a test that writes into a fixture dir while a pass runs and ends with the newer
bytes in place, never the older ones; `DESIGN.md` gains the tier next to "Safety invariants"
with exactly what it does not promise.

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
keeps today's meaning. `dunnage worktree add` (T26) gets it for free. Needs T28's owner and
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
no glob crate — and a per-family `ecosystems` list narrower than the global one. Needs T28.
Done: `tests/cmd` snapshots for a fixture with many build dirs; a skipped position is neither
reported as work nor touched; `docs/usage.md` documents both.

### T40. Known build dirs: a persisted inventory

Every run walks the roots to find build dirs, and in a monorepo the cost of that walk is the
source tree, not the build dirs. The daemon holds the list in memory; the CLI starts from
nothing each time. Persist what discovery found next to the hash index — build dir, adapter,
owner, marker stamp — re-validate entries by their markers, and walk only on a slow cadence,
on `--rediscover`, or when a root's own mtime says something moved. Measure first: the task
starts with a walk benchmark on a large checkout and closes with "not needed" if discovery is
already a small share of a settled re-run. Needs T28.

### T41. Expire the hash index

`src/index.rs` says it itself: entries of deleted targets are never expired. For a CLI run now
and then that is a slowly growing file; under a daemon that visits every unit after every build
it grows for as long as the machine lives. A last-seen field per entry, entries not seen for a
configurable time dropped on save, the file format version bumped with a silent rebuild from an
old file. Free to start. Done: a test ages entries and sees them go; an old-format index is
read as empty rather than as an error.

### T35. Go: `GOCACHE` and `GOMODCACHE`

Lowest priority in the plan. Go has no per-project build dir: `GOCACHE` is one content-addressed
store the `go` command trims on its own, so `dedupe`, `seed`, `orphans` and `evict` have nothing
to do (`docs/ecosystems.md`). What is left is `compress`, through T33's mode: `GOCACHE` found by
`go env GOCACHE`, and `GOMODCACHE` as the counterpart of `--cargo-home` — extracted sources,
which went down 69% for cargo. `GOMODCACHE` dirs are read-only on purpose; lifting and restoring
directory modes is the risk the spike has to price, and "leave `GOMODCACHE` alone" is an
acceptable outcome. Oracle: `go build ./...` after the pass reports every package cached
(`go build -x` runs no compile step) and `go mod verify` is green.
