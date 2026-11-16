# Working on haest

- **Port, don't reinvent.** Behaviour comes from the TypeScript haest at
  `~/projects/remote-claude/src/haest-agent-host/` (`core/`, `providers/`, `tools/`).
  Read the TS file and its test first; keep its doc comments' intent.
- **The contract is in `haest-core`** (`messages`, `provider`, `tool`, `fs`,
  `workspace`, `events`, `deps`, `agent`). Change it only when a port truly
  needs it, and say so in the commit message.
- **Agnostic.** haest ships to anyone: no person's name, no host-specific path or
  product name in code, prompts, defaults or fixtures. A host injects what it needs.
- **One name per concept**, across every crate. "Host" means only the app that
  embeds haest (a CLI, a web app, a test). What a host implements is named for
  what it is, never "host": `AgentHooks` is its say in the loop,
  `ToolServices` what it offers every tool (publish, inner calls, debug lines),
  `SubagentParent` what the subagent hub builds a subagent from, `ModUi` how
  mods reach the human, and haest-runtime's `Ui` how the runtime does.
  `RuntimeHooks` is haest-runtime's own implementation of the loop's hooks.
- **Errors are types.** No `Result<_, String>` and no `anyhow` in a library's
  public signatures: a `thiserror` enum with one variant per case a caller can
  act on, each carrying the values it names (`TaskError::UnknownId { id,
  existing }`), and the source error when there is one, as its source
  (`#[source]`, `#[from]` or a field named `source`). Its `Display` is its
  own words, never its source's: a variant with a source does not print it,
  or the chain says that cause twice (`ConfigError::Read` is
  `"{path}"`, not `"{path}: {source}"`). Show any error to the human or the
  model with `haest_core::error::report`, which walks the chain; `{e}` alone
  drops the cause. A trait a host implements may return
  `haest_core::error::BoxError` (`Box<dyn Error + Send + Sync>`) when haest
  only shows the error (`Mod`'s events, `AgentHooks::before_model_call`).
  A private parser may keep `String`.
  Binaries (haest-cli) use `anyhow`.
- **One path per item.** A library's modules are private and everything a host
  uses is re-exported at its root, once (haest-core, whose modules are the
  contract, is the exception). `pub` only for what a host or a plug-in author
  uses; what only the haest crates share goes in haest-core's `__private`. A
  re-exported crate (`reqwest` in haest-http, `rmcp` in haest-mcp) is part of
  the API: its breaking release is a breaking release of haest.
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
