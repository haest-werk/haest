# haest

One Rust agent loop and tool harness. A model call, the tools it asks for, the
results fed back, until the model answers without tools. Two hosts embed it:
the `haest` CLI and brain.

It is a port of the TypeScript haest in remote-claude (`src/haest-agent-host`),
which remote-claude's agent host still runs.
Session files keep the TS haest's JSON shape, so either haest resumes the other's.
Decisions and status live in
[PLAN.md](https://github.com/haest-werk/haest/blob/main/PLAN.md), working rules in
[CONTRIBUTING.md](https://github.com/haest-werk/haest/blob/main/CONTRIBUTING.md),
what each release changed in
[CHANGELOG.md](https://github.com/haest-werk/haest/blob/main/CHANGELOG.md).

On crates.io as [`haest`](https://crates.io/crates/haest) (docs on
[docs.rs](https://docs.rs/haest/)), source at
[github.com/haest-werk/haest](https://github.com/haest-werk/haest/).

## Crates

| Crate | What |
|---|---|
| `haest-core` | the loop and the contracts: messages, `Provider`, `Tool`, `Fs`, `Workspace`, `AgentHooks`, events; turn guard, caps, ledger, session log, compaction, task board |
| `haest-http` | the HTTP seam (`HttpClient`) the model calls, GenerateImage and the GitHub tracker send through, and its default over reqwest and rustls; the one place haest builds a reqwest client. WebFetch sends through a guarded client of its own, MCP servers through rmcp's |
| `haest-providers` | Anthropic Messages, OpenAI Chat Completions (any compatible server, OpenRouter), OpenAI Responses; `providers.json`; the model catalog; requests over any `HttpClient` |
| `haest-workspace` | the real disk and bash, or a bashkit sandbox over a copy-on-write overlay (or over any `Fs`) whose edits are a change set the human reviews and applies |
| `haest-tools` | Read, Write, Edit, Glob, Grep, Bash (+ background jobs), WebFetch, tasks, ToolSearch, Skill, Agent / SendMessage / StopAgent, GenerateImage |
| `haest-mcp` | MCP client: stdio and streamable HTTP servers, their tools as haest tools |
| `haest-code` | embedded QuickJS: the Code tool and the sandbox `js` command |
| `haest-mods` | mods: change how haest behaves without forking it, in Rust or as `.js`/`.ts` files |
| `haest-ts` | the one TypeScript stripper Code, `js` and mods share |
| `haest-settings` | the settings as plain typed values (`Settings::bare`), no files, no environment, no flags |
| `haest-config` | the opt-in loader, a builder (`Loader`): TOML files, `HAEST_*` variables and flags in the order a host adds them, profiles, trust; plus what the runtime is handed beside the settings (keys, `providers.json`, `.mcp.json`, a child's environment) |
| `haest-runtime` | a whole conversation built from what a host hands it, behind its `Ui`; what the CLI is made of |
| `haest-wire` | the wire format another program reads and drives a conversation with: the envelope around its events (`v`, `sid`, `seq`, `ts`, `path`), the requests a peer sends, a replay ring with gaps, an NDJSON codec; serde only |
| `haest-session` | a conversation another program watches and drives while it runs: the session driver (one turn at a time, input mid-turn steered in, the rest queued through the turn gate), its handle (`Backend`), a numbered stream every subscriber reads with catch-up and one `gap` when it falls behind, approval asks answered over it |
| `haest-transport` | the session served to another program: one connection's rules (`hello`, a reply per request, the stream as asked for, the access check) over any line source and sink, and `Peer`, the asking side; `Stdio` for a parent process (feature `stdio`), `UnixSocket` for programs on this machine (feature `unix`) |
| `haest` | the facade a host depends on: `use haest::...` |
| `haest-cli` | the `haest` binary |
| [`haest-issues`](https://github.com/haest-werk/haest/tree/main/crates/haest-issues/) | experimental, and an example of how to write tools: issue-tracker tools over one trait, for GitHub Issues and markdown board cards. Not in the `haest` facade: depend on it directly |

## haest features

`haest::core` is always there: its key types at the root (`haest::Agent`,
`haest::Tool`) and in `haest::prelude` (glob-import that one), its modules under
`haest::core` (`haest::core::fs`, `haest::core::session`). The rest is opt-in,
each part a module of its name (`haest::workspace` is always the workspace crate):

| Feature | Brings | Default |
|---|---|---|
| `http` | `haest::http` (the HTTP seam: `HttpClient` and its reqwest default; not WebFetch's or MCP's clients); on with `providers` and `tools` | yes |
| `providers` | `haest::providers` | yes |
| `workspace` | `haest::workspace` | yes |
| `tools` | `haest::tools` | yes |
| `mcp` | `haest::mcp` | yes |
| `code` | `haest::code` (the Code tool, the sandbox `js` command) | yes |
| `mods` | `haest::mods` (mods, approvals, plan mode) | yes |
| `settings` | `haest::settings` (the values, plain) | yes |
| `config` | `haest::config` (the loader: files, variables, flags, profiles, trust) | no |
| `runtime` | `haest::runtime` (the conversation built from what it is handed) and everything above but `config` | no |
| `testing` | `haest::core::testing` (a scripted model that records every request, for a host's tests) | no |
| `wire` | `haest::wire` (the envelope, requests, replay ring, NDJSON codec) | no |
| `session` | `haest::session` (the session driver, its handle and stream); with `runtime`, the whole conversation as one | no |
| `transport` | `haest::transport` (the session served to another program, over any line source and sink) | no |
| `transport-stdio` | `haest::transport::Stdio` (a parent process drives the conversation over stdin and stdout) | no |
| `transport-unix` | `haest::transport::UnixSocket` (programs on this machine watch or drive the conversation over a Unix socket) | no |

```toml
haest = "0.2"
# the whole conversation with haest's own files, variables and flags (the CLI's way)
haest = { version = "0.2", features = ["runtime", "config"] }
# a host with its own HTTP client, files and tools (the notebook example)
haest = { version = "0.2", default-features = false, features = ["providers", "workspace", "tools", "settings"] }
```

## Run the CLI

```sh
cargo install haest-cli               # puts `haest` on your PATH
export OPENROUTER_API_KEY=...         # openrouter: models; anthropic: takes ANTHROPIC_API_KEY, openai: OPENAI_API_KEY
haest -p "say hi"                     # bare: a model and nothing else
haest --tools read,grep,glob -p "what is this project?"
haest --profile claude                # a conversation with what haest reads of Claude Code
haest --resume                        # continue the latest session in this directory
haest --stdio                         # driven by a parent process: wire requests on stdin, frames on stdout
haest ctl events <id> --follow        # watch a haest running with control_socket = "watch" (haest ctl list finds it)
haest settings                        # every setting, its value and where it came from
haest models                          # the models haest knows
```

From a checkout, `cargo run -p haest-cli --` takes the same arguments. Every
provider's key and endpoint variable is in
[docs/environment.md](https://github.com/haest-werk/haest/blob/main/docs/environment.md#provider-keys).

## Settings

haest starts bare: no tools, no instruction files, no skills, no MCP, no mods,
no web search, in the sandbox. Every brake is on: approvals, turn caps and
conversation caps. A run gets what you turn on.

Every setting has one name, written three ways:

| Where | Spelled | Example |
|---|---|---|
| `config.toml` | snake_case | `skill_catalog = true` |
| the environment | `HAEST_` + upper snake | `HAEST_SKILL_CATALOG=1` |
| the command line | kebab-case | `--skill-catalog` |

The `haest` command reads these layers, each winning over the ones before it:

1. the bare defaults (`$XDG_DATA_HOME` / `$XDG_CONFIG_HOME` move their dirs)
2. `~/.config/haest/config.toml` (`$XDG_CONFIG_HOME/haest`; `HAEST_CONFIG_DIR` or `--config-dir` moves it)
3. the project's `.haest/config.toml` (the nearest from here up), only inside `trusted_dirs`
4. the profile (`profile = "claude"`, `HAEST_PROFILE`, `--profile`)
5. `HAEST_*` variables
6. flags

`haest settings` shows each value and the layer it came from; it takes the same
flags (`haest settings --profile claude`). `haest --help` lists every flag with
its variable. A switch alone is on (`--mcp`); `--mcp=false` turns off what a
file turned on (the `=` is required, so a prompt after the flag stays the
prompt). A list is a comma list or a TOML array, plus `all` and `none`. An
unknown key or a bad value is an error that names the file, variable or flag.

```toml
# ~/.config/haest/config.toml
tools = ["read", "grep", "glob", "bash"]
context = ["agents"]
trusted_dirs = ["~/projects"]

[env]
GH_TOKEN = "${GITHUB_TOKEN}"

[profiles.review]
tools = ["read", "grep", "glob"]
```

The rest is in `docs/`:

| Doc | What |
|---|---|
| [docs/settings.md](https://github.com/haest-werk/haest/blob/main/docs/settings.md) | every setting: what it does, what it takes, its default, where it may be set, and an example in all three spellings; the built-in profiles. Written from the registry, so it is always the code's |
| [docs/environment.md](https://github.com/haest-werk/haest/blob/main/docs/environment.md) | the variables haest reads (`HAEST_*`, `HOME`, the XDG dirs, provider keys), what a process haest starts gets (`env_passthrough`), passing a variable on purpose (`-E`, `[env]`), `${NAME}` in config files, recipes |
| [docs/examples/](https://github.com/haest-werk/haest/tree/main/docs/examples/) | a whole `config.toml`, a project's `.haest/config.toml`, `.mcp.json`, `providers.json` and `HAEST_*` exports, each loaded by a test |

### Trust

A repo's own files are someone else's code. Its `.haest/config.toml`, its
`.mcp.json` (it names commands to start) and its `.haest/mods` (they run) are
read only when the working dir is inside a `trusted_dirs` entry. `trusted_dirs`
comes from your own `config.toml`, `HAEST_TRUSTED_DIRS` or `--trusted-dirs`,
never from a project's file or a profile: a repo cannot trust itself.
`config_dir` comes only from the environment or a flag. Codex does the same
(`projects.<path>.trust_level`).

### The `claude` profile

`--profile claude` turns on what haest reads of Claude Code: AGENTS.md and the
CLAUDE.md family, skills from `.agents`, `.claude` and installed plugins, the
skill catalog, every built-in tool, subagents, native web search, the project's
MCP servers (in a trusted dir), the real workspace, and your whole environment
for what haest starts (`env_passthrough = all`). It is NOT parity: haest
does not read Claude Code's agents, commands, hooks, permissions, user or
plugin MCP servers, `@` imports, rules, memory or output styles.

That context costs tokens on every call. One "reply ok" turn on this machine
(2026-11-10, claude-sonnet-5.5): bare sent 179 input tokens ($0.0004),
`--profile claude` sent 45,764 ($0.11).

## Background jobs, sessions, files

Background jobs (`run_in_background` on Bash, with BashOutput and KillShell)
exist only in the real workspace; haest kills them when it exits. A job that
exits, a subagent's message for main or a background subagent's answer is a
turn you did not start: during a turn it is read at the turn's next step; at
the prompt it is announced above the line you are typing and runs when you
press Enter. `haest -p` waits for background subagents before it exits.

More providers go in `<config dir>/providers.json`
([an example](https://github.com/haest-werk/haest/blob/main/docs/examples/providers.json)), your mods in
`<config dir>/mods`; sessions are saved under `~/.local/share/haest/sessions`
(`data_dir`). In the REPL, `/help` lists the commands (`/plan` included).

## Embed it

A host builds `AgentDeps` (the agent's tools and system prompt, and the
`SharedDeps` every agent of the conversation shares: models, workspace, hooks
and the rest), makes an `Agent` and calls `run(input, &mut sink)` once per
human turn. Every place a host differs is a trait: `HttpClient` for how
requests travel, `Fs` and `Workspace` for files and shell, `Tool` and
`ReadIntercept` for tools, `AgentHooks` for saving each message and compacting
before a model call, `ModelFacts` for what the host knows of each model,
`TurnCapsSource` for each turn's caps, `Clock` for the time a turn's cap reads,
`EventSink` for events, `SessionSink` for session records, `SubagentParent` for
subagents on haest's hub (`start_member` runs one on a run of the host's own,
or refuses it), `AgentDirectory` for a hub of the host's own.

Nothing in the library reads a file, a variable or a flag of its own accord.
A host writes its settings out:

```rust
use haest::settings::{BuiltinTools, Settings};

let settings = Settings {
    tools: BuiltinTools { read: true, bash: true, ..BuiltinTools::NONE },
    agents: true,
    ..Settings::bare("/work", "/home/me")
};
```

or opts into haest's own files, variables and flags with the `config` feature,
picking the layers and their order. Each layer wins over the ones before it;
a layer left out is not read:

```rust
use haest::config::Loader;

let loaded = Loader::new(Settings::bare(&cwd, &home)) // the defaults: bare, or the host's own
    .xdg_dirs(&env)     // the dirs under $XDG_DATA_HOME, $XDG_CONFIG_HOME
    .user_file()        // <config dir>/config.toml; .file(path) for one the host names
    .project_file()     // the project's .haest/config.toml, inside trusted_dirs only
    .profile()          // the profile `profile` names
    .env(&env)          // HAEST_* variables (and the provider keys)
    .flags(&flags)      // --model, -e, ...
    .load(&fs)
    .await?;
```

That chain is `Loader::standard(defaults, &env, &flags)`, what the `haest`
command reads. `loaded` holds the settings and everything the runtime is handed
beside them. `haest::runtime::build` takes a `RuntimeInputs`: the settings, the
`World` (the real disk with the environment a child gets, or the host's own
`Fs`, where no real process starts), the variables passed on purpose, a
`KeySource` for provider keys (a closure, a map, a vault), the custom
providers, the MCP servers, the OpenRouter routing and the `HttpClient` the
model calls go through. What is not in there does not exist for haest. It
builds a whole conversation behind a `Ui` and an approval `Prompter` -- the CLI
is that plus a terminal. The host runs it through the `Runtime`: `human_turn`
for what the human types, `next_agent_turn` for what a job or a subagent left
in the inbox, `shut_down` at the end. In between it changes it with
`set_model_spec`, `set_effort`, `clear` (a fresh session file) and
`rescan_skills`, each applied to the agent, the runtime's hooks and the session
file together. A host that owns some parts takes the runtime's pieces, or only
`haest::tools::builtin_tools_where(|w| settings.tools.has(w), ..)`.

[`crates/haest/examples/notebook`](https://github.com/haest-werk/haest/tree/main/crates/haest/examples/notebook/)
is a host that brings its own parts (a knowledge base whose node runs the
agent). It wires all of them against fakes (no network, no keys) and asserts
each one. Its settings are written out in Rust, one per kind of run; no
`HAEST_*` variable reaches it:

```sh
cargo run -p haest --example notebook
```

`cargo test` runs it as well. A host that takes haest's own pieces where the
notebook brings its own -- its own `Provider`, a `ShellCommand` in the sandbox shell,
a tool that titles its calls, haest's `SubagentHub` running its members in
process (no member hooks), steering a running turn, a hook that ends the turn, its own skills
behind haest's Skill tool (`SkillSet`, `InlineSkills`), its own tools held back
behind ToolSearch with a list that always loads (`DeferredTools`,
`ToolDeferral::deferring_all_except`) -- is
[`crates/haest/tests/plug_in.rs`](https://github.com/haest-werk/haest/blob/main/crates/haest/tests/plug_in.rs), from
outside the crate, the public API only.

## License

MIT, see [LICENSE](https://github.com/haest-werk/haest/blob/main/LICENSE).
