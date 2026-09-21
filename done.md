

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


#### Execution plan

1. `seed::positions(checkout)`: for every sibling checkout of the family, `eco::discover` its
   build dirs; keep those at their adapter's default place (`build_dir(owner) == dir`) whose
   owner belongs to that sibling (not to a worktree nested in it); the same project path in
   `checkout` must be a dir with no build dir yet. Per position, the sibling built most recently.
2. `Session::seed`: with no `--from` and a checkout root as the dir, every position under one run
   lock, returning `Vec<Seeding>`; otherwise today's single seed. `worktree_add` from a checkout
   root seeds every position, from a subdir only its own.
3. CLI prints one line per position; exit 2 when any source unit was busy.
4. `tests/monorepo.rs`: two workspaces seeded from two different siblings, the project absent on
   this branch left alone. `tests/worktree.rs`: a real two-workspace repository, `worktree add`
   from the root, the oracle (third-party units fresh) for both workspaces.
5. `docs/usage.md`, `DESIGN.md` seed section.

#### Result

- `seed::positions(checkout)` and `seed::Position { project, eco, source }`: every build dir of
  every sibling found by the shared walk, at its adapter's default place, owned by that sibling
  (a worktree nested in it is its own sibling), whose project dir exists in the checkout with no
  build dir yet; per position the sibling whose units were used last.
- `Session::seed` returns `Vec<Seeding>`. In a checkout root with no `--from` it seeds every
  position under one run lock and is an error when there is none; otherwise it is the single
  seed it was. `WorktreeAdded::seeding` is a `Vec` too: from the checkout root every position,
  from a subdir its own. The CLI prints one line per position; exit 2 when any source unit was
  busy.
- Docs: `docs/usage.md` (`seed`, `worktree add`), `DESIGN.md` seed section,
  `docs/architecture.md`.

#### Verified

`just check` (160 tests) and `just check-cross` green. `tests/monorepo.rs`: `api` comes from the
worktree that built it last, `cli` from the main checkout, `legacy` (absent on the new branch) is
left alone, a second seed has nothing to do. `tests/worktree.rs`: `worktree add` from the root
of a real two-workspace repository seeds both, and cargo reports the vendored dependency fresh in
both — the oracle. `~/.cache/dunnage` absent.

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

#### Result

- `Ecosystem::manifest(project)` (cargo: `Cargo.toml`); `inventory::Target::project_gone` when
  it is missing from a checkout that is otherwise there (JSON `project_gone`).
- `orphans::Reason::{CheckoutGone, ProjectGone { manifest, idle_days }}`, printed with every
  removal. A gone project goes only with `--orphans-project-idle-days N` /
  `[orphans] project-idle-days`, which needs `--lossy orphans`, and only when the newest
  `last_used` of its locked profiles is N days old; no `last_used` means not idle. Under the
  lock the manifest must still be missing, so a switch back to the branch keeps the target.
- Without the threshold: reported only, `PROJECT GONE` in `status` and a note in `advise`.
- The out-of-tree half (`build.build-dir` targets getting an owner) is T38.1: the build dir
  records no owner, and how to learn it is the creator's call.
- Test fixtures' `fake_target` writes a `Cargo.toml` next to its target.
- Docs: `README.md`, `docs/usage.md`, `DESIGN.md` orphans section, `docs/architecture.md`.
- `todo.md`, emptied by accident in the T33 commit, restored.

#### Verified

`just check` (164 tests) and `just check-cross` green. `tests/orphans.rs`: a gone project idle
ten days loses its target with a seven-day threshold while a file next to it stays; built two
days ago, or with no threshold, it is only reported (`PROJECT GONE` in `status`, `planned 0`);
a `Cargo.toml` back between the inventory and the lock keeps the target; the threshold without
`--lossy orphans` is an error. The worktree orphan tests still pass unchanged.
`~/.cache/dunnage` absent.

### T30. Swift: SwiftPM `.build/` (DerivedData split off as T30.1)

The most promising target after cargo: APFS is where the tool is strongest, DerivedData runs to
tens of GB, and the only known cure is deleting it. SwiftPM first — `.build/` sits in the
package like `target/` does, and SwiftPM refuses a second instance on it, so there is a lock to
find. DerivedData second: `info.plist` records `WorkspacePath`, which makes `orphans` and
`evict` direct; it needs T29. Spike: confirm the lock file and the call, the plist keys on the
current Xcode, the compress and dedupe yield, and an oracle (`swift build` twice, the second
compiles nothing). Reading a plist may need a crate or `plutil`; a new dependency is the
creator's call at claim time. `seed` does not apply — the dir name is a hash of the path.

#### Result

- Spike (Swift 6.4, Xcode toolchain): `swift build` holds `flock` on
  `<temp dir>/<scratch path, / as _>.lock` (TSCBasic `FileLock`, last 255 bytes of the name) for
  the whole command; `.build/.lock` is only a pid note. swiftbuild writes into `.build/out`, the
  native build system into `.build/<triple>`. `out/CompilationCache.noindex` holds mmapped,
  sparse databases of 12–25 GiB logical size. A switch between debug and release recompiles by
  itself; only `swift build -v` names compile tasks.
- `src/eco/swiftpm.rs`, registered after cargo: claim `.build` with `workspace-state.json`,
  owner the package dir, manifest `Package.swift`, units `out/` and `<triple>/`,
  `Guard::Shared` on the temp lock of the canonical scratch path, `CompilationCache.noindex`
  private, clones only, no `seed`.
- `sys::temp_dir`: `TMPDIR`, else `getconf DARWIN_USER_TEMP_DIR` on macOS.
- The engine creates a missing `Guard::Shared` lock file, as the build tool does.
- `model::scan` leaves private dirs out whole.
- `advise` looks at cargo targets only.
- Yield on swift-argument-parser (debug + release): `.build` 356.7 MiB → 157.4 MiB (−55.9%).
- DerivedData is T30.1: none exists here and none may be made in the real `~/Library`.
- Docs: `README.md`, `docs/usage.md`, `DESIGN.md` SwiftPM section, `docs/bench.md`,
  `toolchain.md` (`swift`, optional).

#### Verified

`just check` (172 tests) and `just check-cross` green. `tests/swiftpm.rs`:
- fixtures: the claim, the units (checkouts, repositories and `index-build` left out), the
  compilation cache never scanned, one lock for all units named in the temp dir, a held lock
  making every unit busy, and a missing lock created;
- with `swift` installed: `swift build` waits while the test holds the lock the adapter names,
  and after dedupe + compress `swift build -v` compiles nothing and the binary runs, while a
  new source mtime does make it compile (the oracle can say no).

By hand on swift-argument-parser: `swift build -c release -v` runs no compile task after the
run, as before it, and the `math` example still adds. `~/.cache/dunnage` is absent, and no
lock file is left in the temp dir.

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

#

#### Result

- Spike: the .NET 10.0.401 SDK here fails every build (workload manifests missing; the repair
  changes the system install and was not run). 9.0.306, pinned by `global.json`, builds offline.
  Nothing was downloaded: two apps with a `ProjectReference` to one library stand in for NuGet
  copies.
- `src/eco/dotnet.rs`, registered after SwiftPM:
  - it claims `obj/` with `project.assets.json`, and `bin/` next to such an `obj/`, each as one
    unit;
  - the owner is the project dir, and the manifest the project file that
    `obj/<file>.nuget.dgspec.json` names;
  - `Guard::Quiet`, with `dotnet`, `MSBuild` and `VBCSCompiler` as the tools;
  - `Sharing::ClonesOnly`, so `--link-artifacts` never links here;
  - no `seed`.
- `CoreCompileInputs.cache` survives a same-content, same-mtime replacement: MSBuild builds
  nothing after dedupe + compress.
- The yield is not measured: it needs NuGet packages, and the cache here is empty. This is said
  in `docs/bench.md`.
- `UseArtifactsOutput` is not found yet; `DESIGN.md` says so.
- Docs updated: `README.md`, `docs/usage.md`, the `DESIGN.md` .NET section, `docs/bench.md`, and
  `toolchain.md` (`dotnet` 9, optional).

#### Verified

- `just check` (176 tests) and `just check-cross` pass.
- `tests/dotnet.rs`, fixtures:
  - `bin/` and `obj/` of a restored project are claimed, and those of an unrestored one are not;
  - the manifest is the recorded project file even with a second one next to it, and deleting
    it marks the project gone;
  - hardlinks are refused even when asked for.
- `tests/dotnet.rs`, with a .NET 9 SDK:
  - three projects, built and then aged two days;
  - a control build is a no-op;
  - after compress + dedupe, `dotnet build -v:n` skips every `CoreCompile` and copies nothing,
    and the app still runs;
  - a new source mtime does make it build.
- The same oracle holds by hand after `dunnage run` on the spike fixture.
- `~/.cache/dunnage` is absent.

### T32. C and C++: CMake, Meson and Ninja build dirs

`compress` is the strong case — uncompressed DWARF in objects and static libraries; cargo's own
`.o` files went to ~5%. `orphans` is easier than for cargo: `CMakeCache.txt` records
`CMAKE_HOME_DIRECTORY`, so "the source is gone" is one `stat`. `dedupe` is expected to find
little (absolute paths in objects) and is measured, not assumed; never hardlinks. `seed` does
not apply — the build dir is full of absolute paths — and `advise` recommends `ccache` with
`file_clone = true` for that job instead. No lock: needs T29, and the spike first settles
whether current Ninja takes one. Plain Make has no marker and builds in the source tree: out of
scope. Oracle: `ninja -n` after a pass plans nothing.

#### Execution plan

This machine has `cmake` 3.31 and a C compiler, but no `ninja` and no `meson`, and installing
them is a new program for the creator to approve. So this task covers CMake build dirs made by
any generator and is verified with the Makefiles one. The Ninja lock question, Meson, and a
Ninja oracle are split off as T32.1.

1. `src/eco/cmake.rs`:
   - claim a dir holding `CMakeCache.txt`, as one unit;
   - the owner is `CMAKE_HOME_DIRECTORY` from the cache, and the manifest `CMakeLists.txt`;
   - `Guard::Quiet`, with `cmake`, `ninja`, `make`, `gmake` and `ctest` as the tools;
   - clones only, and no `seed`.
2. Tests (`tests/cmake.rs`):
   - fixtures for the claim, the owner read from the cache, and an owner that is gone;
   - with `cmake` and `cc`, a real project built with `-g`, then aged;
   - the oracle: after compress + dedupe, `cmake --build` builds and links nothing, and the
     binary runs; a new source mtime makes it build.
3. Measure on a fixture; docs (README, usage, DESIGN, bench), `toolchain.md`; T32.1 card.

#### Result

- `src/eco/cmake.rs`: a dir holding `CMakeCache.txt` is one `Guard::Quiet` unit, clones only.
  The owner is the cache's `CMAKE_HOME_DIRECTORY`, the manifest its `CMakeLists.txt`, so
  `project_gone` and orphans work as for other ecosystems. An in-source build (the source dir is
  the build dir or inside it, compared as real paths) is never claimed.
- Registered after .NET. Docs: README, usage, DESIGN (CMake section), bench, `toolchain.md`.
- fmt debug with tests: the build dir went from 157.7 MiB to 52.3 MiB (−66.8%); compress freed
  105.4 MiB, dedupe 2.5 MiB.
- Not done here: the `ccache` hint in `advise`, which reads cargo targets only; Ninja and Meson
  (T32.1).

#### Verified

- `just check` (179 tests) and `just check-cross` pass.
- `tests/cmake.rs`:
  - a build dir next to its source tree is claimed, owned by it, and shows as project gone once
    the source dir is removed;
  - an in-source build, and a build dir holding its source dir, are not claimed;
  - a real Makefiles project with two executables, aged two days: after compress + dedupe,
    `cmake --build` prints no `Building` or `Linking`, both binaries run, and a new source mtime
    makes it build.
- On fmt after `dunnage run`: `cmake --build` builds nothing and 23 of 23 `ctest` tests pass.
- `~/.cache/dunnage` is absent; no build lock files are left in `$TMPDIR`.
