# cargo-tare

A cargo subcommand that shrinks live `target/` directories without slowing builds: transparent
APFS compression, copy-on-write dedupe across targets, clone-seeding of new worktrees, and opt-in
removal of orphaned or idle targets — planned together so the approaches reinforce each other.
Design in `DESIGN.md`, measurements in `docs/research.md`.

| # | Status | Priority | Complexity | Readiness | Agent |
| --- | --- | --- | --- | --- | --- |
| T20 | todo | P1 | 4 | 0% | |
| T21 | todo | P2 | 5 | 0% | |
| T22 | todo | P2 | 3 | 0% | |

Blockers: none. Everything is free to start: the engine (`src/engine.rs`), the inode model
(`src/model.rs`), the inventory with families (`src/inventory.rs`), the hash index
(`src/index.rs`) and the test harness with the freshness oracle (`tests/common/mod.rs`) exist.
T13–T18 were added from the competitor review in `docs/research.md`; each card says which tool
does the same thing today.

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

### T21. Windows: NTFS compression and ReFS block cloning

Compression: NTFS has per-file transparent compression through `FSCTL_SET_COMPRESSION`, and the
allocated size to measure it with comes from `GetCompressedFileSize`. Dedupe: ReFS has block
cloning (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`); NTFS has no copy-on-write at all, so dedupe there is
T22's fallback, not a clone. File identity is `GetFileInformationByHandle`'s volume serial plus file index,
and the build lock stays `File::try_lock`, which is already cross-platform.

`windows-sys` is approved by the creator for this task; it lands in `toolchain.md` and
`rust.md` in the same change that wires it. Done: the pass suite
runs on ReFS in CI, NTFS reports dedupe as unsupported, and paths with drive letters and `\\?\`
prefixes are covered by tests.

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
