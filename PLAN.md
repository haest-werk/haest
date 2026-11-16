# haest in Rust: haest + the haest CLI

**Goal.** One Rust agent loop and tool harness that every haest host embeds:
the `haest` CLI, remote-claude's agent host, and brain. Today the loop is
TypeScript (`remote-claude/src/haest-agent-host`, about 7.3k lines of core, providers
and tools, plus 3.7k of tests), and brain runs a VENDORED copy of it that drifts (pinned at
`797cc9746`, patched in place). One library removes the copy and the drift.

**The port's source of truth** is the TypeScript haest in
`~/projects/remote-claude/src/haest-agent-host/` (`core/`, `providers/`, `tools/`).
Behaviour comes from there, file by file, tests included.

## Decisions

| Code | Decision | Why |
|---|---|---|
| D1 | Standalone repo `~/projects/haest`, a Cargo workspace | Sharable by remote-claude AND brain; remote-claude's repo is too heavy to be a git dependency |
| D2 | Crates: `haest-core`, `haest-providers`, `haest-workspace`, `haest-tools`, `haest-cli`, and `haest` (the facade: `use haest::...`, features pick the parts) | Codex's split (`codex-rs`) is the precedent; a host links only what it uses |
| D3 | The session JSONL is the TS haest's format, byte compatible (camelCase keys, same record kinds) | Either haest resumes the other's session; brain's conversation files keep working |
| D4 | haest-core has its OWN `Fs` trait; `haest-workspace` adapts any `Fs` into bashkit's `FileSystem` | haest-core stays free of a pre-1.0 dependency, and a host's own files (BrainFs) still get a sandboxed bash |
| D5 | P1 default: haest mods run in embedded QuickJS (`rquickjs`), no `node:` APIs | The Code tool needs QuickJS anyway (wave 2) |
| D6 | P2: brain links `haest` IN-PROCESS (`default-features = false`, features `providers`, `workspace`, `tools`, `settings`) and drops the vendored TS haest from its Bun rooms child. Ruled 2026-11-11; it replaces the first default, haest as brain's own child process holding no keys | One process, no wire between brain and its loop. The cost: the loop runs in the process that holds the keys, so the boundary is D15/D16 alone (a run touches only the `World`, `KeySource` and settings brain hands it) |
| D7 | Every brain patch to the vendored copy is a plug point in the library from day 1 (table below) | Otherwise brain forks again |
| D8 | ONE settings loader (`haest-config`): bare < `<config dir>/config.toml` < a trusted project's `.haest/config.toml` < the profile < `HAEST_*` < flags. One name per setting: `snake_case` in TOML, `HAEST_UPPER_SNAKE`, `--kebab-case`. TOML only | The world's convention (Codex, cargo, git); one registry means a new setting is one row, never three parsers |
| D9 | Bare by default: every capability (tools, context files, skills, agents, Code, web search, image, MCP, mods) off until asked for; every brake (approvals, caps, sandbox) on | haest is "the opposite of `--bare`": a run carries only what it was given. Bare is 179 input tokens a turn, the claude profile 45,764 |
| D10 | Profiles: `claude` built in, `[profiles.<name>]` in any config file adds to it or defines a new one; `--profile` / `HAEST_PROFILE` | One switch for "behave like Claude Code", and named sets for a host's kinds of run (brain's `dream`) |
| D11 | The `claude` profile is what haest READS of Claude Code, not parity; the gaps are listed (`profiles.rs`, README) | A profile that claimed parity would lie |
| D12 | A project's `.haest/config.toml`, `.mcp.json` and `.haest/mods` are read only inside `trusted_dirs`, which only the human's own file, env or flags set | A cloned repo must not configure, start commands or run code by itself (Codex's `trust_level`) |
| D13 | AGENTS.md is off in bare too (`context = ["agents"]` turns it on) | D9 has no exceptions |
| D14 | `haest-runtime` composes the conversation from what it is handed; the CLI is the runtime plus a terminal (`Ui`, `Prompter`); a host may take the pieces instead | Every host builds on the same library; the CLI owns display only |
| D15 | The library reads NOTHING it was not handed: no file, variable or flag of its own, no real disk unless handed `World::Disk`, no inherited env for a child. `haest-settings` is plain typed values (`Settings::bare`, word sets that fail to compile on a typo); `haest-config` is the OPT-IN loader (files, `HAEST_*`, flags) whose `Loaded` is passed straight into `RuntimeInputs` | A host with hardcoded settings (brain) needs no loader, and the node's environment cannot change what a run may do. Config files are the loader's; content files (AGENTS.md, skills, sessions, mods) stay the runtime's, through the handed `Fs` |
| D16 | `RuntimeInputs { settings, world, env, keys, providers, mcp_servers, routing }`. `World::Disk { child_env }` or `World::Fs(fs)` (the host's files: the sandbox over them, no real process, no stdio MCP server). Provider keys through a `KeySource` (a closure, a map, a vault). Trust is the loader's: it lists a project's MCP servers and turns its mods on only in a trusted dir | Each thing a run may touch is a value the host chose, so a test can prove it (`haest-runtime` builds a whole conversation over a `MemFs` with no keys) |
| D17 | `env_passthrough = none \| core \| all`, default `core`; the `claude` profile sets `all`. Same words and `core` list as Codex's `shell_environment_policy.inherit` (whose default is `all`). With no `PATH` handed in, a program is looked up on `/usr/bin:/bin`, as `execvp` does | A key in the human's shell stays with haest unless asked; `none` still runs bash |
| D18 | The loader is a builder: `Loader::new(defaults)` then the layers in the order they win (`.xdg_dirs .user_file .file .project_file .profile .env .flags`), `.load(fs)` last. A layer not added is not read; `Loader::standard` is the CLI's chain. Built on our own registry, not `config` (config-rs) or figment: those replace only the merge (40 lines) and need serde on every settings type, where `flatten` cannot go with `deny_unknown_fields`, so a typo would pass | .NET's `ConfigurationBuilder` shape: the order is the code, readable, and a host takes only the sources it wants |
| D20 | `env`: variables passed on purpose, docker's `-e` as `-E/--env NAME=VALUE` (or `NAME` for haest's own value), repeatable; `HAEST_ENV` holds one; a file's `[env]` table. One registry row of a new kind (`Kind::Vars`), so the flag, the variable and the key come from the same place; layers add name by name, each name keeping its own source. `${NAME}` / `${NAME:-default}` are filled in at load (the `.mcp.json` expander); one with no value and no default is not passed, with a note. Handed as `RuntimeInputs::passed_env`: real processes get it over `child_env`, the sandbox's shell gets it alone. `haest settings` and `get` show names only | A key reaches a tool under the name the tool wants without `env_passthrough = all`, and is never printed |

## Crate map

| Crate | What | Ported from |
|---|---|---|
| `haest-core` | messages, Provider/Tool/Fs/Workspace/AgentHooks contracts, the loop, guard, limits, ledger, media, effort, session log, compaction, system prompt, skills, task board, deferred tools | `core/*.ts` |
| `haest-http` | the HTTP seam: `HttpClient` (one `send`), `Request`, `Response`, `HttpError`; the default `ReqwestClient`; the one place a reqwest client is built and TLS set up. WebFetch keeps a guarded client of its own and MCP servers rmcp's, whatever client the host hands in | new (T2) |
| `haest-providers` | Anthropic Messages, OpenAI Chat Completions (any compatible endpoint), OpenAI Responses, OpenRouter routing + web search, `providers.json`, the model catalog, requests over any `HttpClient` | `providers/*.ts`, `shared/haest-models.ts` |
| `haest-workspace` | real disk + real bash; bashkit sandbox over a copy-on-write overlay; `/changes` + `/apply`; net guard | `core/workspace-*.ts`, `overlay-changes.ts`, `shell-words.ts`, `fs-walk.ts`, `net-guard.ts` |
| `haest-tools` | Read, Write, Edit, Glob, Grep, Bash (+ background jobs), WebFetch, tasks, ToolSearch, Skill, Agent tools (`SubagentHub`, `AgentDirectory`, `AgentToolOptions`), GenerateImage | `tools/*.ts` |
| `haest-mcp` | MCP client over `rmcp`: stdio + HTTP servers, their tools as haest tools | `core/mcp.ts` |
| `haest-code` | the code VM (`rquickjs`), the Code tool over sibling tools, the sandbox `js` command | `core/code-vm*.ts`, `tools/code*.ts` |
| `haest-mods` | mods in QuickJS: commands, prompt rewrite, tool wrapping, approvals, plan mode | `core/mods.ts` |
| `haest-ts` | the ONE TypeScript stripper (`swc` strip_types) used by Code, `js` and mods | -- |
| `haest-settings` | typed `Settings` (`Settings::bare`), `BuiltinTools` / `ContextSources` / `SkillSources` word sets; no I/O | new (D15) |
| `haest-config` | the opt-in loader: the registry (one row per setting), TOML/env/flag layers with provenance, profiles, trust, `env_passthrough`; reads the keys, `providers.json`, routing and a trusted `.mcp.json` for the runtime | new (D8, D15) |
| `haest-runtime` | the conversation built from `RuntimeInputs`: providers, workspace, tools, MCP, mods, subagents, inbox, session file, behind a `Ui` | the CLI's composition, moved (D14, D16); `host/runtime.ts` |
| `haest-cli` | the `haest` binary on `haest-config` + `haest-runtime`: flags generated from the registry, `haest settings`, print mode, `--json` NDJSON events, a REPL, sessions + resume | new (the TS haest has no CLI; its host is the broker) |
| `haest` | the one dependency a host takes; features `http`, `providers`, `workspace`, `tools`, `mcp`, `code`, `mods`, `settings` (default), `config`, `runtime` | -- |
| `haest-issues` | EXPERIMENTAL, an example of how to write tools: a `Tracker` trait, a GitHub Issues backend (REST, token handed in) and a board-card backend (markdown + frontmatter files, the board's own bytes), seven Issue* tools over either; not in the facade | new; the card format from remote-claude's `project-card-file.ts` |

## Brain's plug points (from its vendored patches, `brain/web/src/rooms/agent/haest/README.md`)

| Brain patch | Rust plug point |
|---|---|
| `tool_call_start` forwarded as a loop event | `ProviderEvent::ToolCallStart` -> `LoopEvent::ToolCallStart` (in the contract) |
| `clock` in the deps (a paused run's pause doesn't count) | `SharedDeps::clock` (a `Clock`) |
| openai-compat with no SDK client: the transport is the node's gateway on files.sock | haest-http's `HttpClient` trait (default: reqwest), handed to the providers; brain's goes over its unix socket |
| no OpenRouter provider routing (the gateway decides) | routing is an option on the compat provider, off by default for a custom transport |
| workspace = BrainFs (just-bash over it, writes staged for the asker); dream runs with none | `Fs` trait + `haest-workspace` sandbox over any `Fs`; `ShutWorkspace` |
| Read: `createReadTool(intercept)` (brain media caps, `attachment:<id>`) | Read tool takes an optional intercept |
| Bash without background jobs | jobs are an option of the Bash tool |
| session-store: record types + replay only, the node writes the file | `SessionRecord` + `replay` are separate from the file writer (`SessionSink`) |
| agent tools with brain's own hub (`subagent_type`, sub-agent runs on the node) | `AgentDirectory` (brain's hub) + `AgentToolOptions` (the `subagent_type` enum, the model text) |
| brain decides each run's tools and caps itself | `settings::Settings` written out in Rust, one per kind of run (no loader, nothing read) + `tools::builtin_tools_where` |

Beyond brain's patches: `ShellCommand` (haest-core) + `SandboxWorkspace::with_command`
let a host add its own commands to the sandbox shell (the CLI adds `js`). Every
sandbox has `xan` (`sandbox/xan/`, 2026-11-11): the CSV verbs TS haest's just-bash had
and brain's skills teach (headers, count, head, select, filter, sort, groupby, agg,
stats, frequency) with the part of moonblade's expressions they need; bashkit has
only a line-based `csv`.
Every row is exercised by `crates/haest/examples/brain/` against fakes
(`cargo run -p haest --example brain`, and under `cargo test`). A second
host that takes haest's own hub, shell commands and steering instead is
`crates/haest/tests/plug_in.rs`.

## Waves

| Wave | Work | Status |
|---|---|---|
| 0 | Repo, workspace, the haest-core contract (messages, provider, tool, fs, workspace, events, deps, agent shell) | done |
| 1 | T3 loop core, T4 session/compaction/context, T5 providers, T6 workspaces, T7 tools -- in parallel, one worktree each | done |
| 1b | T8 `haest` CLI on top of wave 1, verified against a real model | done |
| 2 | MCP client (`rmcp`), code VM + Code tool (`rquickjs`), mods (QuickJS), background jobs, Agent tools (sub-agents), Image tool | done; in the CLI too (jobs, subagents, GenerateImage, inbox turns), each checked live |
| 3 | Brain plug-in proof (custom Transport, Fs, tools, ShutWorkspace, clock, tool_call_start, in one example), parity run, README | done |
| 4 | Settings + runtime (D8-D14): `haest-settings`, `haest-runtime`, the CLI rebuilt on them, the brain example on `settle`; checked live (bare vs `--profile claude` tokens, an untrusted project file skipped, a trusted one read) | done |
| 5 | Nothing ambient (D15-D17): `haest-settings` plain values, the loader split out as `haest-config`, `RuntimeInputs` + `World` + `KeySource` + `ChildEnv`, `env_passthrough`, brain's settings written out; checked live (real bash under `none` / `core` / `all`: 5 / 14 / 109 variables, the API key only under `all`) | done |
| 6 | The loader as a builder (D18): `Loader`, one merge over ordered layers (`stack.rs`), the CLI on `Loader::standard` | done |
| 7 | Cleanup pass (/simplify, then a review agent): word lists declared once on their type; `builtin_tools_where` no longer drops `tasks` + `skill` (bug); session listing reads 16 KB a file (doubling to 256 KB) and a resume stops at its match; haest-config parses a file into one `Layer`, `provider_routing` is a registry setting; one `FILE_TOOLS`, `now_ms`, OpenRouter URL; `build()` split (`store.rs`, `runtime.rs`), MCP and mods start beside file discovery, skill roots worked out once; `Runtime::{set_model_spec, set_effort, clear, rescan_skills}`. Review fixes: everything that can fail (workspace, prompt, the new session's file) runs before an MCP server or mod starts; the session header write is awaited, so `clear` either switches or changes nothing; listing ties come in id order; a config under a file counts as absent; `word_enum!` writes the serde spelling; one session path (`session_file`); checked live (print, resume, sessions, routing source, the REPL's /model /effort /clear /skills) | done |
| 8 | `env` (D20): `-E/--env`, `HAEST_ENV`, `[env]` with `${NAME}` filled in; the sandbox shell takes variables (`SandboxWorkspace::with_env`); `.mcp.json` and `env` share one unset-variable note | done |
| 9 | Settings docs: every registry row carries an example; `docs/settings.md` is written from the registry (a test fails when it drifts, `HAEST_BLESS=1` rewrites it) and each example reads the same as a file key, a variable and a flag; `docs/environment.md` (what haest reads, what a child gets, `-E`, `${NAME}`); `docs/examples/` loaded by tests | done |

### Parity (2026-11-10)

Every real TS haest session on this machine (14 files in `~/.rclaude/haest/sessions`)
was replayed by both haests (TS `SessionStore.load`, Rust `session::replay`) and the
results compared field by field: **14 of 14 identical**. The run found one Rust bug
(a float one bit off, fixed in 9e3607a). The port also found and fixed four TS haest
bugs, shipped in remote-claude: the spill (2), absolute-path Glob, a custom
`type: anthropic` provider dropping its headers, and Read numbering a phantom line
after a closing newline.

### Open

| Code | Gap |
|---|---|
| G1 | The auto-mode judge is not ported |
| G2 | The sandbox overlay lives in one process: a resumed sandbox session starts from a clean overlay |
| G3 | MCP and compaction pass their tests but have not run against a live server / a real long session |
| G4 | `mv` of a directory across bashkit mounts fails |
| G5 | A PDF returned by an MCP tool stays text; it does not become media |
| G7 | The `claude` profile does not read Claude Code's agents, commands, hooks, permissions, user or plugin MCP servers, `@` imports, rules, memory or output styles (D11) |

Not in this plan (the consumers, each its own job once the library holds):
remote-claude's broker-facing host in Rust (TS `host/`, 3.3k lines, 18 wire
types), and brain switching from its vendored TS copy to the crate.

## Done means

- `cargo test --workspace` green, `cargo clippy --workspace --all-targets` clean.
- Every ported TS test has a Rust counterpart (behaviour, not wording).
- `haest -p "..."` runs a real multi-step task with tools against a real model,
  and `haest --resume <id>` continues it from the session file.
- A TS haest session file replays in the Rust haest.
- The brain example compiles and runs against fakes using only plug points.

## Risks

| Code | Risk | Handling |
|---|---|---|
| R1 | bashkit is pre-1.0 | Resolved for the fs seam: `bashkit::FileSystem` is a public trait and `Bash::builder().fs(..)` takes a custom one. Pin the version; it sits behind haest's own `Fs` (D4) |
| R2 | the model catalog exists twice (TS + Rust) | Generated JSON from `shared/haest-models.ts` for now; later remote-claude reads haest's |
| R3 | parity drift while the TS haest keeps changing | Port from a named commit, record it per crate, diff upstream before each wave |
