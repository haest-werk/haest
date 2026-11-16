# Working on haest

- **Port, don't reinvent.** Behaviour comes from the TypeScript haest at
  `~/projects/remote-claude/src/haest-agent-host/` (`core/`, `providers/`, `tools/`).
  Read the TS file and its test first; keep its doc comments' intent.
- **The contract is in `haest-core`** (`messages`, `provider`, `tool`, `fs`,
  `workspace`, `events`, `deps`, `agent`). Change it only when a port truly
  needs it, and say so in the commit message.
- **Agnostic.** haest ships to anyone: no person's name, no host-specific path or
  product name in code, prompts, defaults or fixtures. A host injects what it needs.
- **Wire-compatible serde.** Anything that lands in a session file or an NDJSON
  event keeps the TS haest's JSON shape (camelCase keys, same tags).
- **Tests defend behaviour.** A test fails only when behaviour changed: never
  assert wording, formatting or source text. Port every TS test as a behaviour test.
- **Files stay small.** About 300 lines per `.rs` file, tests included in a
  `#[cfg(test)] mod tests` or a sibling `tests.rs`. Over that, split by concern.
- **Dependencies** a single crate uses go in THAT crate's `Cargo.toml`, not the
  workspace table (parallel work never collides there).
- **Gates before every commit:** `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` all clean (the doctests are in it), and the docs
  build the way docs.rs builds them, without a warning:
  `RUSTDOCFLAGS="-D warnings --cfg docsrs" cargo +nightly doc --workspace --all-features --no-deps`.
- **Docs are part of the API.** Every crate has `#![warn(missing_docs)]`: a
  public item gets a doc that says what it is for in plain words, and the types
  a host constructs or implements get a runnable example. Links between items
  are intra-doc links (`[`Agent`]`); a link to a file in the repo is an absolute
  `https://github.com/haest-werk/haest/blob/main/...` URL, because crates.io and
  docs.rs resolve a relative one against the wrong folder.
- **Worktrees** for parallel work live in `.claude/worktrees/<slug>` on a branch
  `wt/<slug>`; integrate with `git rebase main` then a fast-forward merge on `main`.
- **Commits:** `git commit -F - <<'EOF'` (quoted heredoc, never `-m "..."`), a
  conventional subject (`feat(core): ...`), no `Co-Authored-By` or other trailers.
