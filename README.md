# cargo-tare

Tare: the weight of the packaging, not the goods. `cargo-tare` takes the dead weight out of Cargo
`target/` directories — without deleting what you still build with and without slowing builds.

Status: early. `status`, `advise`, `seed`, compress, dedupe and the opt-in `orphans` / `evict` /
`incremental` work; the rest is in `plan.md`.

## Why

On the machine this was designed for: 44 target dirs, ~158 GB. 114 GB of that sat in 30 worktree
checkouts git no longer knew about; nothing flags those. Inside the live targets, age-based
cleaners find ~50 MB of 14 GB, and `cargo-cache` only cleans `~/.cargo`. The live bytes are
duplicated and highly compressible:

- `deps/` compresses to ~20–30% of its size (object files to ~5%);
- 36% of bytes across sibling targets are identical content;
- a new worktree rebuilds every third-party crate from scratch.

Measured on a real 587-crate workspace (`docs/bench.md`): two freshly built targets go from
3.63 GiB to about 0.93 GiB of blocks on disk — 74% — and cargo afterwards reports not one unit
out of date. Incremental builds stay where they were, within the noise.

## How

One planner, several approaches that reinforce each other (details in `DESIGN.md`):

- **compress** — transparent APFS compression of stable artifacts;
- **dedupe** — identical files across and inside targets become copy-on-write clones;
- **seed** — a new worktree's target starts as a zero-byte clone of a sibling's, so third-party
  crates are not rebuilt;
- **orphans / evict** — opt-in removal of targets whose worktree is gone, idle targets, and
  least-recently-built targets above a global size cap.

Everything runs under cargo's own build lock, preserves mtimes so nothing is rebuilt, and treats
hardlink groups as one unit.

## Usage

Works today:

```
cargo tare status ~/code            # read-only: every target under the root
cargo tare status --cargo-home ~/code  # and what the registry sources weigh
cargo tare status --json ~/code
cargo tare run --dry-run ~/code     # plan only
cargo tare run ~/code
cargo tare run [--dry-run] [--pass <PASS>]... [--lossy <PASS>]... [--index <FILE>]
               [--config <FILE>] [--json] [<ROOT>...]
cargo tare advise ~/code            # read-only: what makes these targets bigger
cargo tare seed [--from <DIR>] [--dry-run] [--index <FILE>] [<DIR>]
              [--min-age <SECS>] [--min-size <BYTES>]
cargo tare run --cargo-home ~/code     # and the registry sources in ~/.cargo
cargo tare run --dry-run --lossy orphans ~/code
cargo tare run --dry-run --lossy evict --evict-idle-days 30 ~/code
cargo tare run --lossy evict --evict-max-total-gib 50 ~/code
```

`status` lists cargo target dirs grouped by family (a repository and its worktrees): size on disk
as `du` counts it, days since the last build, `ORPHANED` for a worktree git no longer knows, and
totals — bytes not compressed yet and an upper bound of what dedupe could share.

`run` applies two lossless passes. **compress**: files of 8 KB and more get transparent APFS
compression (LZFSE); hardlink groups stay groups. **dedupe**: files with equal content become
copy-on-write clones of one copy, compressed if that copy is. Targets are compared inside a
family, which is where most duplicates are. What the compression backend refused, and why, is
printed at the end. A `<ROOT>` is searched for targets; a target dir itself works too.

It takes cargo's own lock, skips profile dirs with a running build, leaves alone files younger
than one hour or too small to win a block (8 KB for compress, 4 KB for dedupe), works on private
copies and swaps them in with `rename`, keeps mtimes so nothing is rebuilt, and remembers content
hashes in `~/.cache/cargo-tare/hashes-v1.bin` so the next run reads only new files.
Only dirs carrying cargo's own `CACHEDIR.TAG` count as targets. `--lossy` enables a
pass that deletes rebuildable data; lossless passes need no flag. How the engine keeps a target
safe is described in `DESIGN.md`, "Engine" and "Safety invariants".

`--pass <PASS>` runs only the passes you name (`orphans`, `evict`, `incremental`, `doc`,
`compress`, `dedupe`), which
is how the benchmarks tell them apart. `--min-age` and `--min-size` move the two floors below;
they exist for measurements, and the defaults are what `docs/bench.md` justifies.

**orphans** deletes, so it is off unless you name it: `--lossy orphans` removes the whole
`target/` of a checkout that is a git worktree the repository no longer registers (its `.git`
file points at a missing worktree record). Nothing outside `target/` is touched — such a
checkout can hold work git can no longer report. No threshold, and every removal is printed
with its reason on a dry run too.

**seed** copies instead of deleting. In a fresh worktree, `cargo tare seed` clones the target of
a sibling checkout of the same repository — the one built most recently, at the same place
inside it — into yours. On APFS every file is a `clonefile`, so the new target shares its blocks
with the old one and costs no disk space until something rewrites it. `incremental/`, the lock
files and leftover temp files stay behind, a profile dir with a running build is reported and
not copied, and a checkout that already has a target is refused rather than merged into.

How much of the first build it saves depends on what moved: units whose absolute path changed
(the workspace members, path dependencies) are compiled again, everything else is reused. The
test suite measures it against an empty target rather than assuming it.

**incremental** deletes, so it is off unless you name it: `--lossy incremental
--incremental-idle-days <N>` drops `target/<profile>/incremental/` in profile dirs with no build
for N days. Cargo writes that cache for workspace members only, and it is not part of a
fingerprint: right after the pass nothing is stale at all, and the cost is one non-incremental
rebuild the next time you edit a crate in that workspace.

**doc** deletes, so it is off unless you name it: `--lossy doc` removes `<target>/doc`, the
rustdoc output `cargo doc` writes again from scratch and no build reads — `cargo clean --doc` by
another name. It goes only while this tool holds the target's build locks and nothing in the
target is being built, and its size is reported like every other removal.

**evict** deletes, so it is off unless you name it: `--lossy evict` plus `--evict-idle-days <N>`
(profile dirs such as `target/debug` with no build for N days), `--evict-max-total-gib <N>`
(then the least recently built, until everything under the roots fits), or both. Only whole
profile dirs go, only under cargo's lock, never one with a running build or one built since the
run started looking. Every removal is printed with its reason; `--dry-run` prints the same list
and removes nothing. Cargo rebuilds what was removed on the next build of that profile.

Add `--evict-whole-target` and a target whose every profile dir is being evicted goes whole, so
`doc/`, `package/`, `tmp/` and `CACHEDIR.TAG` leave with it instead of surviving as an empty
shell. One profile with a build running, or one built since the run started looking, keeps the
target dir and the free profiles are evicted on their own. Only `target/` is ever removed; the
sources next to it are not.

`--across-families` compares every target under the roots with every other instead of one
repository at a time. Unrelated projects do share bytes — the same crate, the same version, the
same features — and the hash index already holds what it takes to find them. The price is a
wider lock: the run holds every target's build locks for its whole length, so it is off by
default and belongs in a scheduled run rather than between two builds. Measured
(`docs/bench.md`): about half of a freshly built target was already on disk in an unrelated
project — 172.6 MiB of 351.8 MiB — and nothing went stale.

`status` and `advise` also report what a toolchain upgrade left behind: cargo records the
compiler that built each unit in its own fingerprints, so units whose rustc is no longer the one
in use are countable without parsing a single hashed file name. The bytes are an estimate — the
profile's size in the share of those units — because a fingerprint does not name the files it
produced. Nothing removes them: `cargo clean`, or a rebuild, is the only cure today.

`--cargo-home [DIR]` adds the cargo home to the run as a group of its own: the extracted
registry sources (`registry/src`) and git checkouts (`git/checkouts`) are compressed like any
other stable artifact. Without a value the flag resolves `CARGO_HOME`, else `~/.cargo`. Only
compress runs there — the `.crate` archives in `registry/cache` and cargo's `registry/index` are
left alone — and the whole group is taken under cargo's own `<home>/.package-cache` lock, so a
running `cargo fetch` stops the pass instead of racing it. Nothing is deleted and no file's
content or mtime changes, which is what decides whether cargo unpacks a crate again; it does not.
Measured on a clone of a real home (`docs/bench.md`): 1.52 GiB of registry sources down to
469 MiB, 69% off, and the build afterwards reports nothing stale and unpacks nothing again.
The run needs no target dirs of its own, so `cargo tare run --cargo-home` alone is a valid run,
and `cargo tare status --cargo-home` reports the same dirs without touching them (it is opt-in
because measuring them costs a second walk).

**advise** changes nothing: it reads the manifests and cargo configs of the projects it finds
and names what makes their targets bigger than they need to be — full debuginfo where
`line-tables-only` would do, dependency debuginfo nobody steps into, a missing `strip` in
release, `[unstable]` keys a stable toolchain ignores without a word, and
`cache.auto-clean-frequency`, which cargo has cleaned the cargo home by since 1.88. Every finding
names the file and the key it is about, including keys the file does not have yet, because
cargo's own defaults are part of the problem. Below them come the things only the inventory
shows: what `incremental/` weighs here, families whose targets could share a `[build] build-dir`,
checkouts that `seed` would fill, and orphaned worktrees. `--json` prints the same as data.

`--json` prints the same report as one JSON document instead of the table. Exit codes: `0`
everything the run planned was done, `1` the run failed (bad flags, bad config, I/O), `2` a
profile dir was skipped because a build held its lock — what a scheduled run needs to tell
"nothing to do" from "come back later".

## Configuration

`$XDG_CONFIG_HOME/cargo-tare/config.toml`, or `~/.config/cargo-tare/config.toml`. Every key is
optional and every flag wins over the file; `--config <FILE>` reads another file instead, and a
file named there must exist. An unknown key stops the run rather than being ignored.

```toml
roots = ["~/code"]          # what `run` and `status` search when the command line names none
lossy = ["orphans"]         # lossy passes to enable, as `--lossy` would; thresholds still apply
min-age = 3600              # seconds; both lossless passes
across-families = true      # compare targets of different repositories too
min-size = 8192             # bytes; both lossless passes

[evict]
idle-days = 30
max-total-gib = 50
whole-target = true         # take the target dir itself once all of its profiles are evicted

[incremental]
idle-days = 7

[family."/Users/me/code/monorepo/.git"]
skip = true                 # never touch this repository and its worktrees
```

A family is a repository and its worktrees, keyed by the git common dir `status` prints (a target
without a repository is its own family). `skip` is the only per-family key: the `evict` cap and
the idle rules are decided over everything under the roots at once, so they stay global.

Planned:

```
cargo tare advise          # config findings
```

## Platforms

The tool builds and runs on macOS, Linux and Windows. What it *does* depends on what the
filesystem can do, and it never pretends:

| | macOS | Linux | Windows |
| --- | --- | --- | --- |
| `status`, `advise`, `seed` | yes | yes | yes |
| `dedupe` (block sharing) | yes, APFS | yes on btrfs, XFS (`reflink=1`), bcachefs | not yet — ReFS is `T21` |
| `compress` | yes, APFS/LZFSE | yes on btrfs | not yet — NTFS is `T21` |

The question is the filesystem, not the operating system, so the tool asks yours instead of
guessing from its name: it writes a small temp file, tries to clone it and tries to set the
compression flag, and removes both. `status` tells you the answer:

```
  1.4 GiB  built 2d ago  /home/you/project/target
           this filesystem neither shares blocks nor compresses: both lossless passes find nothing here
```

A pass that cannot win anything there plans nothing and says so — it does not copy files around
for no gain. ext4 and NTFS have neither block sharing nor compression at all; the fallback for
them is hardlinking (`T22`).

`seed` works everywhere: where the filesystem shares blocks the copy is free, where it does not
it costs the disk but still saves the build, and it says which of the two happened.

Version-gated features (unit-level pruning, shared build-dir automation) are in `roadmap.md`.

## Running it automatically

The lossless passes are safe to run unattended: they never delete, they skip a profile dir with a
running build, and exit code `2` says a build was in the way. A `just` recipe after a build:

```just
tare:
    cargo tare run ~/code || test $? -eq 2
```

Or on macOS, every night, with launchd (`~/Library/LaunchAgents/dev.cargo-tare.plist`):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>Label</key><string>dev.cargo-tare</string>
  <key>ProgramArguments</key>
  <array><string>/usr/bin/env</string><string>cargo</string><string>tare</string>
         <string>run</string><string>/Users/me/code</string></array>
  <key>StartCalendarInterval</key><dict><key>Hour</key><integer>3</integer></dict>
  <key>Nice</key><integer>10</integer>
</dict></plist>
```

Put the roots and the lossy passes in the config file rather than in the plist, so the same
schedule follows what you change there. Load it with `launchctl load -w <path>`.
