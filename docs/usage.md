# dunnage — user guide

How to install the tool, what to run first, and what each command does. Why it works the way it
does is in `DESIGN.md`; the numbers behind the defaults are in `docs/bench.md`.

## Install

The crate is not on crates.io yet. Build it from a checkout, with the toolchain the repository
pins (`rust-toolchain.toml`):

```
git clone https://github.com/listepo/dunnage
cargo install --locked --path dunnage
```

The binary is `dunnage`; `dunnage --version` tells you it is on the `PATH`. If you prefer
`cargo dunnage <command>`, link it once under the name cargo looks for:

```
ln -s "$(command -v dunnage)" "$(dirname "$(command -v dunnage)")/cargo-dunnage"
```

What it can do depends on the filesystem under your target dirs, not on the operating system:

| Filesystem | `compress` | `dedupe` | `seed` |
| --- | --- | --- | --- |
| APFS (macOS) | yes | yes, clones | free clone |
| btrfs | yes | yes, clones | free clone |
| XFS with `reflink=1`, bcachefs | no | yes, clones | free clone |
| ext4 and the rest | no | cargo home sources only, as hardlinks | a real copy |
| NTFS, ReFS | not yet (`T21`) | not yet (`T21`) | a real copy |

`status` says which row you are on; nothing has to be configured.

## The first five minutes

Everything here is read-only until the last step.

```
dunnage status ~/code              # 1. what is there and what it weighs
dunnage advise ~/code              # 2. what makes it bigger than it needs to be
dunnage run --dry-run ~/code       # 3. what a run would do
dunnage run ~/code                 # 4. do it
```

1. `status` finds every cargo target dir under the root, groups them by repository (a *family*:
   one repository and its worktrees), and prints the size on disk, the days since the last
   build, `ORPHANED` for worktrees git no longer knows, and how much is still uncompressed or
   could be shared.
2. `advise` reads manifests and cargo configs and names the keys that inflate the targets —
   debuginfo levels, a missing `strip`, ignored `[unstable]` keys — with the file each one
   belongs in.
3. `run --dry-run` prints the full plan and touches nothing.
4. `run` compresses and deduplicates, repeating both until nothing is left for them. It deletes
   nothing, rebuilds nothing, and can be repeated at any time; a later run is fast because
   content hashes are cached, and finds only what builds wrote since.

Check the result with `dunnage status ~/code` again, and with `cargo build` in any of the
projects: it must report nothing to recompile.

## What is safe and what deletes

| Pass | Kind | What it does | Enabled |
| --- | --- | --- | --- |
| `compress` | lossless | transparent filesystem compression of files ≥ 8 KB older than 1 h | always |
| `dedupe` | lossless | equal files become copy-on-write clones of one copy | always |
| `orphans` | **deletes** | the whole `target/` of a worktree git no longer registers | `--lossy orphans` |
| `evict` | **deletes** | profile dirs idle for N days, or the least recently built above a size cap | `--lossy evict` + a threshold |
| `incremental` | **deletes** | `incremental/` of profile dirs idle for N days | `--lossy incremental --incremental-idle-days N` |
| `doc` | **deletes** | `<target>/doc` | `--lossy doc` |

Lossless passes never change a file's content or modification time, which is all cargo looks at,
so nothing is rebuilt. A lossy pass removes only what cargo can build again, only when you name
it, and prints every removal with its reason — on `--dry-run` too. Only `target/` contents are
ever removed; sources are never touched.

Every pass takes cargo's own build lock. A profile dir with a build running is skipped and
reported, and the exit code says so.

## Commands

### `dunnage status [--json] [--cargo-home [DIR]] [ROOT]...`

Read-only inventory. `ROOT` defaults to the current directory; a target dir itself works too.
`--cargo-home` adds the unpacked registry sources and git checkouts in `~/.cargo` (or
`$CARGO_HOME`, or `DIR`) to the report, at the price of a second walk. `--json` prints the same
as one JSON document.

### `dunnage advise [--json] [ROOT]...`

Read-only findings about manifests and cargo configs, followed by what only the inventory shows:
the weight of `incremental/`, families that could share a `build-dir`, checkouts `seed` would
fill, orphaned worktrees, and units built by a toolchain you no longer use.

### `dunnage run [OPTIONS] [ROOT]...`

Plans and applies the passes, one family at a time. Without a `ROOT` it uses `roots` from the
config file.

| Option | Meaning |
| --- | --- |
| `--dry-run` | print the plan, change nothing |
| `--pass <PASS>` | run only the named passes; repeatable |
| `--lossy <PASS>` | enable a deleting pass; repeatable |
| `--evict-idle-days <DAYS>` | with `--lossy evict`: profile dirs not built for this long |
| `--evict-max-total-gib <GIB>` | with `--lossy evict`: then the least recently built, until everything fits |
| `--evict-whole-target` | with `--lossy evict`: remove the target dir itself once all its profiles went |
| `--incremental-idle-days <DAYS>` | with `--lossy incremental` |
| `--min-age <SECS>` | leave younger files alone; default 3600 |
| `--min-size <BYTES>` | leave smaller files alone; default 8192 for compress, 4096 for dedupe |
| `--cargo-home [DIR]` | also compress the cargo home's unpacked sources, under cargo's `.package-cache` lock |
| `--store DIR` | also compress a content-addressed store (`GOCACHE`, `~/.cabal/store`, Zig's `o/`); repeatable, no lock, entries older than an hour only |
| `--across-families` | compare targets of unrelated repositories too; holds every lock for the whole run |
| `--link-artifacts` | **hazard**: on filesystems without clones, share build artifacts as hardlinks |
| `--index <FILE>` | content-hash cache; default `~/.cache/dunnage/hashes-v1.bin` |
| `--config <FILE>` | another config file; it must exist |
| `--json` | the report as JSON |

`--link-artifacts` is off for a reason: rustc rewrites its outputs in place, so a build that
rewrites one linked artifact rewrites it in every target sharing the inode. Use it only for
targets nobody builds in parallel, or not at all.

### `dunnage seed [--from DIR] [--dry-run] [--index FILE] [DIR]`

Fills the empty target of a fresh checkout from a sibling checkout of the same repository, so
the first build does not compile every third-party crate again. `DIR` is the checkout to seed
(default: the current directory); `--from` names the source checkout or target dir, otherwise
the family's most recently built target is used. A checkout that already has a target is
refused. Workspace members and path dependencies are still compiled — their absolute paths
changed — and everything else is reused.

### `dunnage worktree add [--dry-run] [--index FILE] GIT ARGS...`

`git worktree add GIT ARGS...`, then `seed` into the new worktree in one step. Run it from a
workspace inside the repository: the new worktree is seeded at the same relative path, from the
most recently built checkout. Git's own failure is shown as is and nothing is seeded; with no
built checkout to copy from the worktree is still added and the tool says there was nothing to
seed. `--dry-run` still adds the worktree and only reports what seeding would copy. Only the
dependencies whose sources stay where they are — registry crates, a vendor dir outside the
repository — build warm; the workspace itself moved and is compiled again.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | everything planned was done |
| `1` | the run failed: bad flags, bad config, I/O |
| `2` | at least one profile dir was skipped because a build held its lock — run again later |

## Recipes

A new worktree that builds warm:

```
dunnage worktree add ../feature-x -b feature-x
```

or, for a worktree that is already there, `dunnage seed ../feature-x`.

Reclaim the worktrees an agent or a script left behind — look first, then delete:

```
dunnage run --dry-run --lossy orphans ~/code
dunnage run --lossy orphans ~/code
```

Keep all targets under a budget:

```
dunnage run --lossy evict --evict-idle-days 30 --evict-max-total-gib 50 ~/code
```

The registry sources as well (no target dirs needed):

```
dunnage run --cargo-home
```

A script that must tell "nothing to do" from "a build was in the way":

```
dunnage run ~/code || test $? -eq 2
```

Scheduling with `launchd` or a `just` recipe is shown in `README.md`, "Running it
automatically".

## Configuration

`$XDG_CONFIG_HOME/dunnage/config.toml`, else `~/.config/dunnage/config.toml`. Every key is
optional, a flag always wins over the file, and an unknown key stops the run.

```toml
roots = ["~/code"]
lossy = ["orphans"]
min-age = 3600
min-size = 8192
across-families = false
stores = []  # content-addressed stores to compress, as --store does

[evict]
idle-days = 30
max-total-gib = 50
whole-target = true

[incremental]
idle-days = 7

[index]
idle-days = 30

[family."/Users/me/code/monorepo/.git"]
skip = true
```

The family key is the git common dir that `status` prints for the family.
`[index] idle-days` is how long the hash index keeps a file's hash that no run has looked up
(default 30); forgetting one costs a single rehash.

## Troubleshooting

- **`status` finds nothing.** Only dirs carrying cargo's own `CACHEDIR.TAG` count. A target
  created by a very old cargo, or a dir whose tag was deleted, is not recognised on purpose.
- **"this filesystem neither shares blocks nor compresses".** The probe tried both and both
  failed — ext4, NTFS, a network mount. Nothing is planned there rather than copied for no gain.
- **Exit code 2 every time.** Something holds the build lock: a running `cargo build`, a
  `cargo watch`, or rust-analyzer's check. Run when the editor is idle, or schedule it at night.
- **`du` shows no change on btrfs.** btrfs reports uncompressed sizes in `stat`; look at `df` for
  the volume or `compsize` for a directory.
- **Files a build just wrote are skipped.** Expected: `min-age` leaves anything younger than an
  hour alone, because the next build rewrites it anyway.
- **Leftover `.dunnage-tmp-*` files.** A run was killed mid-replace. The original files are intact
  and the next run removes the leftovers.
