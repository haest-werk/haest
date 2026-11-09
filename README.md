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
| `code` | `kern::code` (the Code tool, the sandbox `js` command) | yes |
| `mods` | `kern::mods` (mods, approvals, plan mode) | yes |

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
| `KERN_WEB_SEARCH` | `exa` | the provider's web search: `off`, `exa`, `auto`, `native`, `parallel`, `firecrawl`, `perplexity` |
| `KERN_CODE` | `on` | the Code tool; `KERN_CODE_TOOLS` adds tools it may call (`Name` or `prefix*`) |
| `KERN_APPROVALS` | `on` | ask on the terminal before an irreversible Bash command (real workspace) |
| `KERN_PROJECT_MODS` | `0` | `1` also loads the repo's own mods from `.kern/mods` (they run with your trust) |
| `KERN_MAX_AGENT_DEPTH`, `KERN_MAX_SUBAGENTS` | `2`, `4` | how deep subagents nest and how many run at once; `0` = no Agent tool |
| `KERN_SUBAGENT_USD`, `_STEPS`, `_MINUTES` | `1`, `50`, `15` | one subagent's spend, model calls and wall clock |
| `KERN_MAX_AGENT_TURNS` | `30` | turns in a row a background job or subagent may start with no message from you; past it kern holds them until you post (`0` = no cap) |
| `KERN_IMAGE_MODEL`, `KERN_IMAGE_MODELS` | the library's | GenerateImage's default model and the others a call may pick; the tool exists only with `OPENROUTER_API_KEY` |

Background jobs (`run_in_background` on Bash, with BashOutput and KillShell)
exist only in the real workspace; kern kills them when it exits. A job that
exits, a subagent's message for main or a background subagent's answer is a
turn you did not start: during a turn it is read at the turn's next step; at
the prompt it is announced above the line you are typing and runs when you
press Enter. `kern -p` waits for background subagents before it exits.

More providers go in `~/.config/kern/providers.json` (`KERN_CONFIG_DIR`), your
mods in `~/.config/kern/mods`; sessions are saved under
`~/.local/share/kern/sessions` (`KERN_DATA_DIR`). `cargo run -p kern-cli -- --help`
lists every flag. In the REPL, `/help` lists the commands (`/plan` included).

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
