# AGENTS.md

Notes for coding agents working in this repository.

## Mandatory for every agent

- The human is the only author. No agent adds a Co-Authored-By trailer, a "Generated with …" line
  or itself as author to a commit, merge or PR.
- English for repository files: code, comments, docs, commits, PR text.
- If a directory above this repository contains an `AGENTS.md` or `CLAUDE.md`, follow it too. If it
  conflicts with this file, ask the creator.

## What cargo-tare is

A cargo subcommand that shrinks live `target/` directories without slowing builds. Read
`DESIGN.md` before touching code — the inode model, pass ordering and safety invariants there are
the contract. Measurements that justify the design are in `docs/research.md`.

## Safety rules (the tool mutates build directories)

- Tests and experiments run **only** on fixture targets inside temp dirs. Never point a
  development build of the tool at a real project's `target/`.
- Lossy passes (`orphans`, `evict`, later `prune`) are never enabled by default, in code or in
  test config.
- A lossless pass is not done until the freshness oracle (see `DESIGN.md`) is green for it.
- Do not parse `name-<hash>` artifact file names; cargo's layout is changing (build-dir layout v2).
- Never delete or rewrite anything in the creator's real target dirs while debugging; ask first.

## Commands

- `just check` — `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
  Run it before calling a task done.
- `cargo run -- tare <args>` — cargo invokes the binary as `cargo-tare tare <args>`.

## Working agreements

- Tasks live in `plan.md` (table + cards), mirrored in `todo.md`; finished tasks move whole to
  `done.md`. Claim a task in the table before starting and write the execution plan into its card.
- Version-gated work lives in `roadmap.md`; re-check the cargo changelog before moving an item.
- New dependencies need the creator's approval; candidates are listed in `DESIGN.md`. Keep
  `toolchain.md` in sync with the manifest.
- macOS / APFS is the only supported platform for 0.x; keep platform calls behind the backend
  boundary described in `DESIGN.md`.
