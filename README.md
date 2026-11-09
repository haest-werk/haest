# kern

One Rust agent loop and tool harness. A model call, the tools it asks for, the
results fed back, until the model answers without tools. Three hosts embed it:
the `kern` CLI, remote-claude's agent host and brain.

It is a port of the TypeScript kern in remote-claude (`src/kern-agent-host`).
Session files keep the TS kern's JSON shape, so either kern resumes the other's.
Decisions and status live in [PLAN.md](PLAN.md), working rules in
[CONTRIBUTING.md](CONTRIBUTING.md).

## Crates

| Crate | What |
|---|---|
| `kern-core` | the loop and the contracts: messages, `Provider`, `Tool`, `Fs`, `Workspace`, `AgentHooks`, events; turn guard, limits, ledger, session log, compaction, task board |
| `kern-providers` | Anthropic Messages, OpenAI Chat Completions (any compatible server, OpenRouter), OpenAI Responses; `providers.json`; the model catalog; a pluggable HTTP `Transport` |
| `kern-workspace` | the real disk and bash, or a bashkit sandbox over a copy-on-write overlay (or over any `Fs`); `/changes` and `/apply` |
| `kern-tools` | Read, Write, Edit, Glob, Grep, Bash (+ background jobs), WebFetch, tasks, ToolSearch, Skill, Agent / SendMessage / StopAgent, GenerateImage |
| `kern-mcp` | MCP client: stdio and streamable HTTP servers, their tools as kern tools |
| `kern-code` | embedded QuickJS: the Code tool and the sandbox `js` command |
| `kern-mods` | mods: change how kern behaves without forking it, in Rust or as `.js`/`.ts` files |
| `kern-lib` | the facade a host depends on: `use kern::...` |
| `kern-cli` | the `kern` binary |

## kern-lib features

`kern::core` (re-exported at the root, `kern::Agent`, `kern::fs`, ...) is always
there. The rest is opt-in:

| Feature | Brings | Default |
|---|---|---|
| `providers` | `kern::providers` | yes |
| `workspace` | `kern::workspace` | yes |
| `tools` | `kern::tools` | yes |
| `mcp` | `kern::mcp` | yes |

```toml
kern-lib = { git = "...", package = "kern-lib" }
# a host with its own transport, files and tools (the brain example)
kern-lib = { git = "...", package = "kern-lib", default-features = false, features = ["providers", "workspace", "tools"] }
```

## Run the CLI

```sh
export OPENROUTER_API_KEY=...
cargo run -p kern-cli -- -p "list the files here and say what this project is"
cargo run -p kern-cli                 # a conversation (REPL)
cargo run -p kern-cli -- --resume     # continue the latest session in this directory
cargo run -p kern-cli -- models       # the models kern knows
```

| Variable | Default | What |
|---|---|---|
| `KERN_MODEL` | `openrouter:anthropic/claude-sonnet-5.5` | `provider:model` (also `-m`) |
| `KERN_WORKSPACE` | `sandbox` | `sandbox`: a simulated bash over a copy-on-write overlay, nothing reaches the disk until `/apply`; `real`: the real disk and bash (also `-w`) |
| `OPENROUTER_API_KEY` | -- | for `openrouter:` models; `anthropic:` takes `ANTHROPIC_API_KEY`, `openai:` takes `OPENAI_API_KEY` |

More providers go in `~/.config/kern/providers.json` (`KERN_CONFIG_DIR`);
sessions are saved under `~/.local/share/kern/sessions` (`KERN_DATA_DIR`).
`cargo run -p kern-cli -- --help` lists every flag; each has a `KERN_*` twin.

## Embed it

A host builds `AgentDeps` (models, tools, system prompt, workspace, hooks),
makes an `Agent` and calls `run(input, &mut sink)` once per human turn. Every
place a host differs is a trait: `Transport` for how model calls travel, `Fs`
and `Workspace` for files and shell, `Tool` and `ReadIntercept` for tools,
`AgentHooks` for the clock, persistence and caps, `EventSink` for events,
`SessionSink` for session records, `AgentDirectory` for sub-agents.

[`crates/kern-lib/examples/brain`](crates/kern-lib/examples/brain/) wires all of
them the way brain does, against fakes (no network, no keys), and asserts each
one:

```sh
cargo run -p kern-lib --example brain
```
