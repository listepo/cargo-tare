

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
