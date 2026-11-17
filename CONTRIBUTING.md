# Working on haest

- **Keep the behaviour, don't reinvent it.** What haest does is defined by
  its tests and doc comments: read the code's tests and docs first, and keep
  their intent. haest was ported from a TypeScript original; where a
  maintainer has it, it is the reference for intent, never a requirement
  for a contributor.
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
  The words haest settled on:
  - **cap**: a maximum that stops something (`TurnCaps`, `ConversationCaps`,
    `VmCaps`, `ModCaps`, the settings' `CapSettings`). "Limit" is only
    `remedy::Limit`, which also covers `SandboxEdits`, no cap.
  - **`model_spec`**: the `provider:model` string, in every field, parameter
    and setter; `model` is the provider's own id (`ResolvedModel::model`).
  - **`*Options`** is what a host hands a constructor in Rust
    (`GenerateImageOptions`, `ModOptions`); **`*Config`** names only an entry
    read from a file (`McpServerConfig`, `CustomProviderConfig`).
  - A tool struct is the name the model calls plus `Tool`
    (`GenerateImageTool`).
  - **`with_*`** is a consuming setter that cannot fail (`self -> Self`,
    `#[must_use]`), and nothing else: a constructor is `new`, `from_*` or
    `new_with_*`, a fallible add `try_*`, a merge is named for what it
    makes (`overlaid`), and what changes every clone of a shared service
    takes `&self` as `set_*` (`Ledger::set_remedies`).
  - A setter for an optional field takes the value (`with_name(impl
    Into<String>)` sets it), never `impl Into<Option<_>>` or `Option<&str>`:
    a caller holding an `Option` calls it only for `Some`. Where a caller
    really needs to clear a field that has a default, that is a setter of
    its own, named for it (`without_app_title`). A flag's setter takes the
    bool (`with_background(bool)`), never a no-argument `in_background()`.
  - A line to the human is **info / warn / error / debug**, on `Ui`, `ModUi`
    and `ModEnv` alike. A tool tells the human what it did in its result;
    the one line `ToolServices` takes is `debug`, for the diagnostic log.
  - **subagent**, one word. `AgentDeps` is one agent's own parts (tools,
    system prompt); `SharedDeps` what every agent of a conversation shares.
- **Errors are types.** No `Result<_, String>` and no `anyhow` in a library's
  public signatures: a `thiserror` enum with one variant per case a caller can
  act on, each carrying the values it names (`TaskError::UnknownId { id,
  existing }`), and the source error when there is one, as its source
  (`#[source]`, `#[from]` or a field named `source`). Its `Display` is its
  own words, never its source's: a variant with a source does not print it,
  or the chain says that cause twice (`ConfigError::Read` is `"cannot read
  {path}"`, not `"cannot read {path}: {source}"`). Show any error to the
  human or the model with `haest_core::error::report`, which walks the
  chain; `{e}` alone
  drops the cause. A trait a host implements may return
  `haest_core::error::BoxError` (`Box<dyn Error + Send + Sync>`) when haest
  only shows the error (`Mod`'s events, `AgentHooks::before_model_call`).
  A private parser may keep `String`.
  Binaries (haest-cli) use `anyhow`.
- **No `Arc`, `Box` or `dyn` in a signature.** A plug point is held in its
  `<Trait>Ref` handle (`plug_ref!`), and a constructor or setter takes
  `impl Into<XRef>`, so a host hands in the value itself. A service a host
  shares is a cheap `Clone` whose clones are the one service, never an
  `Arc` the host wraps. One exception: a function that only borrows a file
  system for the call takes `&dyn Fs` (the net guard `&dyn Resolve`), since
  that borrow takes the value (`&MemFs`) and its handle (`&FsRef`) alike, and
  `&impl Fs` would refuse the handle: a handle never implements its own trait.
  A trait with a blanket impl (`KeySource` and `ModelResolver` for
  closures) is the exception: the compiler tries that impl on the handle
  and refuses it, so `&dyn KeySource` takes a handle as `keys.as_ref()`.
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
- **Lints live in one place:** the root `Cargo.toml`'s `[workspace.lints]`,
  which every crate takes with `[lints] workspace = true` (a new crate too),
  and clippy's settings in `clippy.toml`. No crate sets a lint of its own.
  The table keeps clippy's default groups and adds picks from restriction
  and pedantic, each group under a line that says why. When a lint is wrong
  for one item, put `#[expect(lint, reason = "...")]` on that item, the
  narrowest one that holds it; never a bare `#[allow]` (an `expect` warns
  once it is no longer needed). The one exception is a test-support module
  every test binary includes: each uses a different part, so it takes an
  inner `#![allow(dead_code, reason = "...")]`. Unsafe code is denied: a
  site that needs it claims it with an `#[expect(unsafe_code, reason = "...")]`.
- **A wrapper names every method.** A type that implements a trait over an
  inner value of the same trait (`ModTools` over a `ToolSource`, a subagent's
  hooks over the conversation's) puts `#[warn(clippy::missing_trait_methods)]`
  on that impl. Each method then forwards, or says why it does not: a method
  the trait gains later fails the gates there instead of quietly running the
  default in place of the inner value's. A tool over a tool implements
  `ToolWrapper` instead and names only what it changes: its blanket `Tool`
  impl is the one that takes the lint.
- **Docs are part of the API.** The lint table warns on a public item without
  docs: it gets a doc that says what it is for in plain words, and the types
  a host constructs or implements get a runnable example. Links between items
  are intra-doc links (`[`Agent`]`); a link to a file in the repo is an absolute
  `https://github.com/haest-werk/haest/blob/main/...` URL, because crates.io and
  docs.rs resolve a relative one against the wrong folder.
- **Worktrees** for parallel work live in `.claude/worktrees/<slug>` on a branch
  `wt/<slug>`; integrate with `git rebase main` then a fast-forward merge on `main`.
- **Commits:** `git commit -F - <<'EOF'` (quoted heredoc, never `-m "..."`), a
  conventional subject (`feat(core): ...`), no `Co-Authored-By` or other trailers.
