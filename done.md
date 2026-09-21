

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
