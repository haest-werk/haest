# kern in Rust: kern-lib + the kern CLI

**Goal.** One Rust agent loop and tool harness that every kern host embeds:
the `kern` CLI, remote-claude's agent host, and brain. Today the loop is
TypeScript (`remote-claude/src/kern-agent-host`, about 7.3k lines of core, providers
and tools, plus 3.7k of tests), and brain runs a VENDORED copy of it that drifts (pinned at
`797cc9746`, patched in place). One library removes the copy and the drift.

**The port's source of truth** is the TypeScript kern in
`~/projects/remote-claude/src/kern-agent-host/` (`core/`, `providers/`, `tools/`).
Behaviour comes from there, file by file, tests included.

## Decisions

| Code | Decision | Why |
|---|---|---|
| D1 | Standalone repo `~/projects/kern`, a Cargo workspace | Sharable by remote-claude AND brain; remote-claude's repo is too heavy to be a git dependency |
| D2 | Crates: `kern-core`, `kern-providers`, `kern-workspace`, `kern-tools`, `kern-cli`, and `kern-lib` (the facade: `use kern::...`, features pick the parts) | Codex's split (`codex-rs`) is the precedent; a host links only what it uses |
| D3 | The session JSONL is the TS kern's format, byte compatible (camelCase keys, same record kinds) | Either kern resumes the other's session; brain's conversation files keep working |
| D4 | kern-core has its OWN `Fs` trait; `kern-workspace` adapts any `Fs` into bashkit's `FileSystem` | kern-core stays free of a pre-1.0 dependency, and a host's own files (BrainFs) still get a sandboxed bash |
| D5 | P1 default: kern mods run in embedded QuickJS (`rquickjs`), no `node:` APIs | The Code tool needs QuickJS anyway (wave 2) |
| D6 | P2 default: brain runs the Rust kern as its OWN child process that holds no keys, replacing the Bun rooms child; the library also allows linking in-process | Brain's trust boundary ("kern for the loop, Rust for the trust boundary") stays as built |
| D7 | Every brain patch to the vendored copy is a plug point in the library from day 1 (table below) | Otherwise brain forks again |
| D8 | ONE settings loader (`kern-config`): bare < `<config dir>/config.toml` < a trusted project's `.kern/config.toml` < the profile < `KERN_*` < flags. One name per setting: `snake_case` in TOML, `KERN_UPPER_SNAKE`, `--kebab-case`. TOML only | The world's convention (Codex, cargo, git); one registry means a new setting is one row, never three parsers |
| D9 | Bare by default: every capability (tools, context files, skills, agents, Code, web search, image, MCP, mods) off until asked for; every brake (approvals, caps, sandbox) on | kern is "the opposite of `--bare`": a run carries only what it was given. Bare is 179 input tokens a turn, the claude profile 45,764 |
| D10 | Profiles: `claude` built in, `[profiles.<name>]` in any config file adds to it or defines a new one; `--profile` / `KERN_PROFILE` | One switch for "behave like Claude Code", and named sets for a host's kinds of run (brain's `dream`) |
| D11 | The `claude` profile is what kern READS of Claude Code, not parity; the gaps are listed (`profiles.rs`, README) | A profile that claimed parity would lie |
| D12 | A project's `.kern/config.toml`, `.mcp.json` and `.kern/mods` are read only inside `trusted_dirs`, which only the human's own file, env or flags set | A cloned repo must not configure, start commands or run code by itself (Codex's `trust_level`) |
| D13 | AGENTS.md is off in bare too (`context = ["agents"]` turns it on) | D9 has no exceptions |
| D14 | `kern-runtime` composes the conversation from what it is handed; the CLI is the runtime plus a terminal (`Ui`, `Prompter`); a host may take the pieces instead | Every host builds on the same library; the CLI owns display only |
| D15 | The library reads NOTHING it was not handed: no file, variable or flag of its own, no real disk unless handed `World::Disk`, no inherited env for a child. `kern-settings` is plain typed values (`Settings::bare`, word sets that fail to compile on a typo); `kern-config` is the OPT-IN loader (files, `KERN_*`, flags) whose `Loaded` is passed straight into `RuntimeInputs` | A host with hardcoded settings (brain) needs no loader, and the node's environment cannot change what a run may do. Config files are the loader's; content files (AGENTS.md, skills, sessions, mods) stay the runtime's, through the handed `Fs` |
| D16 | `RuntimeInputs { settings, world, keys, providers, mcp_servers, routing }`. `World::Disk { child_env }` or `World::Fs(fs)` (the host's files: the sandbox over them, no real process, no stdio MCP server). Provider keys through a `KeySource` (a closure, a map, a vault). Trust is the loader's: it lists a project's MCP servers and turns its mods on only in a trusted dir | Each thing a run may touch is a value the host chose, so a test can prove it (`kern-runtime` builds a whole conversation over a `MemFs` with no keys) |
| D17 | `env_passthrough = none \| core \| all`, default `core`; the `claude` profile sets `all`. Same words and `core` list as Codex's `shell_environment_policy.inherit` (whose default is `all`). With no `PATH` handed in, a program is looked up on `/usr/bin:/bin`, as `execvp` does | A key in the human's shell stays with kern unless asked; `none` still runs bash |

## Crate map

| Crate | What | Ported from |
|---|---|---|
| `kern-core` | messages, Provider/Tool/Fs/Workspace/AgentHooks contracts, the loop, guard, limits, ledger, media, effort, session log, compaction, system prompt, skills, task board, deferred tools | `core/*.ts` |
| `kern-providers` | Anthropic Messages, OpenAI Chat Completions (any compatible endpoint), OpenAI Responses, OpenRouter routing + web search, `providers.json`, the model catalog, a pluggable HTTP `Transport` | `providers/*.ts`, `shared/kern-models.ts` |
| `kern-workspace` | real disk + real bash; bashkit sandbox over a copy-on-write overlay; `/changes` + `/apply`; net guard | `core/workspace-*.ts`, `overlay-changes.ts`, `shell-words.ts`, `fs-walk.ts`, `net-guard.ts` |
| `kern-tools` | Read, Write, Edit, Glob, Grep, Bash (+ background jobs), WebFetch, tasks, ToolSearch, Skill, Agent tools (`SubagentHub`, `AgentDirectory`, `AgentToolOptions`), GenerateImage | `tools/*.ts` |
| `kern-mcp` | MCP client over `rmcp`: stdio + HTTP servers, their tools as kern tools | `core/mcp.ts` |
| `kern-code` | the code VM (`rquickjs`), the Code tool over sibling tools, the sandbox `js` command | `core/code-vm*.ts`, `tools/code*.ts` |
| `kern-mods` | mods in QuickJS: commands, prompt rewrite, tool wrapping, approvals, plan mode | `core/mods.ts` |
| `kern-ts` | the ONE TypeScript stripper (`swc` strip_types) used by Code, `js` and mods | -- |
| `kern-settings` | typed `Settings` (`Settings::bare`), `BuiltinTools` / `ContextSources` / `SkillSources` word sets; no I/O | new (D15) |
| `kern-config` | the opt-in loader: the registry (one row per setting), TOML/env/flag layers with provenance, profiles, trust, `env_passthrough`; reads the keys, `providers.json`, routing and a trusted `.mcp.json` for the runtime | new (D8, D15) |
| `kern-runtime` | the conversation built from `RuntimeInputs`: providers, workspace, tools, MCP, mods, subagents, inbox, session file, behind a `Ui` | the CLI's composition, moved (D14, D16); `host/runtime.ts` |
| `kern-cli` | the `kern` binary on `kern-config` + `kern-runtime`: flags generated from the registry, `kern settings`, print mode, `--json` NDJSON events, a REPL, sessions + resume | new (the TS kern has no CLI; its host is the broker) |
| `kern-lib` | the one dependency a host takes; features `providers`, `workspace`, `tools`, `mcp`, `code`, `mods`, `settings` (default), `config`, `runtime` | -- |

## Brain's plug points (from its vendored patches, `brain/web/src/rooms/agent/kern/README.md`)

| Brain patch | Rust plug point |
|---|---|
| `tool_call_start` forwarded as a loop event | `ProviderEvent::ToolCallStart` -> `LoopEvent::ToolCallStart` (in the contract) |
| `clock` in the deps (a paused run's pause doesn't count) | `AgentHooks::now_ms` |
| openai-compat with no SDK client: the transport is the node's gateway on files.sock | `kern-providers` `Transport` trait (default: reqwest); brain's goes over its unix socket |
| no OpenRouter provider routing (the gateway decides) | routing is an option on the compat provider, off by default for a custom transport |
| workspace = BrainFs (just-bash over it, writes staged for the asker); dream runs with none | `Fs` trait + `kern-workspace` sandbox over any `Fs`; `ShutWorkspace` |
| Read: `createReadTool(intercept)` (brain media caps, `attachment:<id>`) | Read tool takes an optional intercept |
| Bash without background jobs | jobs are an option of the Bash tool |
| session-store: record types + replay only, the node writes the file | `SessionRecord` + `replay` are separate from the file writer (`SessionSink`) |
| agent tools with brain's own hub (`subagent_type`, sub-agent runs on the node) | `AgentDirectory` (brain's hub) + `AgentToolOptions` (the `subagent_type` enum, the model text) |
| brain decides each run's tools and caps itself | `settings::Settings` written out in Rust, one per kind of run (no loader, nothing read) + `tools::builtin_tools_where` |

Beyond brain's patches: `ShellCommand` (kern-core) + `SandboxWorkspace::with_command`
let a host add its own commands to the sandbox shell (the CLI adds `js`).
Every row is exercised by `crates/kern-lib/examples/brain/` against fakes
(`cargo run -p kern-lib --example brain`).

## Waves

| Wave | Work | Status |
|---|---|---|
| 0 | Repo, workspace, the kern-core contract (messages, provider, tool, fs, workspace, events, deps, agent shell) | done |
| 1 | T3 loop core, T4 session/compaction/context, T5 providers, T6 workspaces, T7 tools -- in parallel, one worktree each | done |
| 1b | T8 `kern` CLI on top of wave 1, verified against a real model | done |
| 2 | MCP client (`rmcp`), code VM + Code tool (`rquickjs`), mods (QuickJS), background jobs, Agent tools (sub-agents), Image tool | done; in the CLI too (jobs, subagents, GenerateImage, inbox turns), each checked live |
| 3 | Brain plug-in proof (custom Transport, Fs, tools, ShutWorkspace, clock, tool_call_start, in one example), parity run, README | done |
| 4 | Settings + runtime (D8-D14): `kern-settings`, `kern-runtime`, the CLI rebuilt on them, the brain example on `settle`; checked live (bare vs `--profile claude` tokens, an untrusted project file skipped, a trusted one read) | done |
| 5 | Nothing ambient (D15-D17): `kern-settings` plain values, the loader split out as `kern-config`, `RuntimeInputs` + `World` + `KeySource` + `ChildEnv`, `env_passthrough`, brain's settings written out; checked live (real bash under `none` / `core` / `all`: 5 / 14 / 109 variables, the API key only under `all`) | done |

### Parity (2026-11-10)

Every real TS kern session on this machine (14 files in `~/.rclaude/kern/sessions`)
was replayed by both kerns (TS `SessionStore.load`, Rust `session::replay`) and the
results compared field by field: **14 of 14 identical**. The run found one Rust bug
(a float one bit off, fixed in 9e3607a). The port also found and fixed four TS kern
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
| G6 | The repo has no git remote |
| G7 | The `claude` profile does not read Claude Code's agents, commands, hooks, permissions, user or plugin MCP servers, `@` imports, rules, memory or output styles (D11) |

Not in this plan (the consumers, each its own job once the library holds):
remote-claude's broker-facing host in Rust (TS `host/`, 3.3k lines, 18 wire
types), and brain switching from its vendored TS copy to the crate.

## Done means

- `cargo test --workspace` green, `cargo clippy --workspace --all-targets` clean.
- Every ported TS test has a Rust counterpart (behaviour, not wording).
- `kern -p "..."` runs a real multi-step task with tools against a real model,
  and `kern --resume <id>` continues it from the session file.
- A TS kern session file replays in the Rust kern.
- The brain example compiles and runs against fakes using only plug points.

## Risks

| Code | Risk | Handling |
|---|---|---|
| R1 | bashkit is pre-1.0 | Resolved for the fs seam: `bashkit::FileSystem` is a public trait and `Bash::builder().fs(..)` takes a custom one. Pin the version; it sits behind kern's own `Fs` (D4) |
| R2 | the model catalog exists twice (TS + Rust) | Generated JSON from `shared/kern-models.ts` for now; later remote-claude reads kern's |
| R3 | parity drift while the TS kern keeps changing | Port from a named commit, record it per crate, diff upstream before each wave |
