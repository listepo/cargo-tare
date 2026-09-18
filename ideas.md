# Ideas

Not approved. Nothing here moves to `roadmap.md` or `plan.md` without the creator's approval.

- Linux backend: `FICLONE` on btrfs / XFS for dedupe and seeding; rely on filesystem-level zstd
  instead of a compress pass.
- Watch mode: run the lossless passes when a profile dir's cargo lock is released (FSEvents),
  instead of a timer or a manual run.
- `cargo tare worktree add` wrapper: `git worktree add` + `seed` in one step.
- Park / unpark: `tar | zstd` an idle worktree's target (~75% smaller) and restore it on demand.
- Skip seeding workspace-member artifacts by asking `cargo metadata` for member names.
- Publish to crates.io and the homebrew tap once 0.1 is proven on the measured machine.
- Windows: ReFS block cloning (Dev Drive) as a third backend.

## A CI job on a btrfs loopback image

T20 was verified in a local lima VM by the creator's choice. A GitHub Actions job that creates a
btrfs loopback image, mounts it, points `TMPDIR` at it and runs the suite would keep the Linux
half honest without a VM on hand — the same two-line setup the VM used. Not approved; the local
VM is what the creator asked for.
