# cargo-tare

A cargo subcommand that shrinks live `target/` directories without slowing builds: transparent
APFS compression, copy-on-write dedupe across targets, clone-seeding of new worktrees, and opt-in
removal of orphaned or idle targets — planned together so the approaches reinforce each other.
Design in `DESIGN.md`, measurements in `docs/research.md`.

| # | Status | Priority | Complexity | Readiness | Agent |
| --- | --- | --- | --- | --- | --- |
| T19 | todo | P1 | 4 | 0% | |
| T20 | todo | P1 | 4 | 0% | |
| T21 | todo | P2 | 5 | 0% | |

Blockers: none. Everything is free to start: the engine (`src/engine.rs`), the inode model
(`src/model.rs`), the inventory with families (`src/inventory.rs`), the hash index
(`src/index.rs`) and the test harness with the freshness oracle (`tests/common/mod.rs`) exist.
T13–T18 were added from the competitor review in `docs/research.md`; each card says which tool
does the same thing today.

### T19. Platform layer: build and run on Linux and Windows

The tool is macOS-only today, and not by design: `model.rs`, `inventory.rs`, `engine.rs` and the
two lossless passes reach for `std::os::unix::fs::MetadataExt` (`dev`, `ino`, `nlink`, `blocks`,
`st_flags`) and for macOS's `clonefile` and `UF_COMPRESSED` directly. Windows has none of those
names, so the crate does not even compile there.

First task, and a blocker for T20 and T21: a `src/sys/` module that owns every platform
primitive — file identity, hardlink count, allocated size, the "already compressed" flag, clone
a file, compress a file — with the macOS implementation moved into it unchanged. Everything
above it stays as it is. A platform that cannot clone or cannot compress reports that, and the
pass plans nothing instead of failing: the passes are already allowed to skip.

Done: `cargo check` passes for `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`, the
macOS tests are untouched and still green, and no `std::os::unix` import is left outside
`src/sys/`. CI (T22 if it is not folded in here) builds all three.

### T20. Linux: reflink dedupe and filesystem compression

With T19 in place, fill in the Linux half. Dedupe: `FICLONE` (btrfs, XFS with reflink=1, bcachefs)
is the exact equivalent of `clonefile`; `FIDEDUPERANGE` is the safer variant that verifies the
bytes in the kernel and works even when the target is shared already. `rustix` is already a
dependency and covers both, so no new crate should be needed. Compression: btrfs takes
`chattr +c` (`FS_COMPR_FL`) per file, and only new writes are compressed, so a file has to be
rewritten to shrink — which is what the pass does anyway. ext4 has neither, so both passes must
report "not supported here" rather than pretend.

Done: the pass suite runs on a btrfs loopback image in CI, `ext4` reports the passes as
unsupported instead of failing, and `docs/bench.md` gains a Linux row.

### T21. Windows: NTFS compression and ReFS block cloning

Compression: NTFS has per-file transparent compression through `FSCTL_SET_COMPRESSION`, and the
allocated size to measure it with comes from `GetCompressedFileSize`. Dedupe: ReFS has block
cloning (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`); NTFS has no copy-on-write at all, so dedupe must be
off there — a hardlink is not a substitute, because a build that rewrites one artifact would
rewrite the other. File identity is `GetFileInformationByHandle`'s volume serial plus file index,
and the build lock stays `File::try_lock`, which is already cross-platform.

`windows-sys` is approved by the creator for this task; it lands in `toolchain.md` and
`rust.md` in the same change that wires it. Done: the pass suite
runs on ReFS in CI, NTFS reports dedupe as unsupported, and paths with drive letters and `\\?\`
prefixes are covered by tests.
