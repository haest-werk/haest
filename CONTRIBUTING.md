# Working on kern

- **Port, don't reinvent.** Behaviour comes from the TypeScript kern at
  `~/projects/remote-claude/src/kern-agent-host/` (`core/`, `providers/`, `tools/`).
  Read the TS file and its test first; keep its doc comments' intent.
- **The contract is in `kern-core`** (`messages`, `provider`, `tool`, `fs`,
  `workspace`, `events`, `deps`, `agent`). Change it only when a port truly
  needs it, and say so in the commit message.
- **Agnostic.** kern ships to anyone: no person's name, no host-specific path or
  product name in code, prompts, defaults or fixtures. A host injects what it needs.
- **Wire-compatible serde.** Anything that lands in a session file or an NDJSON
  event keeps the TS kern's JSON shape (camelCase keys, same tags).
- **Tests defend behaviour.** A test fails only when behaviour changed: never
  assert wording, formatting or source text. Port every TS test as a behaviour test.
- **Files stay small.** About 300 lines per `.rs` file, tests included in a
  `#[cfg(test)] mod tests` or a sibling `tests.rs`. Over that, split by concern.
- **Dependencies** a single crate uses go in THAT crate's `Cargo.toml`, not the
  workspace table (parallel work never collides there).
- **Gates before every commit:** `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` all clean.
- **Worktrees** for parallel work live in `.claude/worktrees/<slug>` on a branch
  `wt/<slug>`; integrate with `git rebase main` then a fast-forward merge on `main`.
- **Commits:** `git commit -F - <<'EOF'` (quoted heredoc, never `-m "..."`), a
  conventional subject (`feat(core): ...`), no `Co-Authored-By` or other trailers.
