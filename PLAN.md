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

## Crate map

| Crate | What | Ported from |
|---|---|---|
| `kern-core` | messages, Provider/Tool/Fs/Workspace/AgentHooks contracts, the loop, guard, limits, ledger, media, effort, session log, compaction, system prompt, skills, task board, deferred tools | `core/*.ts` |
| `kern-providers` | Anthropic Messages, OpenAI Chat Completions (any compatible endpoint), OpenAI Responses, OpenRouter routing + web search, `providers.json`, the model catalog, a pluggable HTTP `Transport` | `providers/*.ts`, `shared/kern-models.ts` |
| `kern-workspace` | real disk + real bash; bashkit sandbox over a copy-on-write overlay; `/changes` + `/apply`; net guard | `core/workspace-*.ts`, `overlay-changes.ts`, `shell-words.ts`, `fs-walk.ts`, `net-guard.ts` |
| `kern-tools` | Read, Write, Edit, Glob, Grep, Bash, WebFetch, tasks, ToolSearch, Skill (wave 2: Code, Agent, Image) | `tools/*.ts` |
| `kern-cli` | the `kern` binary: one-shot print mode, `--json` NDJSON events, a REPL, config, sessions + resume, compaction | new (the TS kern has no CLI; its host is the broker) |
| `kern-lib` | the one dependency a host takes | -- |

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
| agent tools with brain's own hub (`subagent_type`, sub-agent runs on the node) | wave 2: Agent tools over a `SubagentHub` trait |

## Waves

| Wave | Work | Status |
|---|---|---|
| 0 | Repo, workspace, the kern-core contract (messages, provider, tool, fs, workspace, events, deps, agent shell) | done |
| 1 | T3 loop core, T4 session/compaction/context, T5 providers, T6 workspaces, T7 tools -- in parallel, one worktree each | running |
| 1b | T8 `kern` CLI on top of wave 1, verified against a real model | next |
| 2 | MCP client (`rmcp`), code VM + Code tool (`rquickjs`), mods (QuickJS), background jobs, Agent tools (sub-agents), Image tool | after 1b |
| 3 | Brain plug-in proof (custom Transport, Fs, tools, ShutWorkspace, clock, tool_call_start, in one example), parity run (same prompts through TS kern and Rust kern), README | after 2 |

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
