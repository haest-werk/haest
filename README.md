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
| `kern-ts` | the one TypeScript stripper Code, `js` and mods share |
| `kern-settings` | the settings as plain typed values (`Settings::bare`), no files, no environment, no flags |
| `kern-config` | the opt-in loader, a builder (`Loader`): TOML files, `KERN_*` variables and flags in the order a host adds them, profiles, trust; plus what the runtime is handed beside the settings (keys, `providers.json`, `.mcp.json`, a child's environment) |
| `kern-runtime` | a whole conversation built from what a host hands it, behind its `Ui`; what the CLI is made of |
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
| `settings` | `kern::settings` (the values, plain) | yes |
| `config` | `kern::config` (the loader: files, variables, flags, profiles, trust) | no |
| `runtime` | `kern::runtime` (the conversation built from what it is handed) and everything above but `config` | no |

```toml
kern-lib = { git = "...", package = "kern-lib" }
# the whole conversation with kern's own files, variables and flags (the CLI's way)
kern-lib = { git = "...", package = "kern-lib", features = ["runtime", "config"] }
# a host with its own transport, files and tools (the brain example)
kern-lib = { git = "...", package = "kern-lib", default-features = false, features = ["providers", "workspace", "tools", "settings"] }
```

## Run the CLI

```sh
export OPENROUTER_API_KEY=...         # openrouter: models; anthropic: takes ANTHROPIC_API_KEY, openai: OPENAI_API_KEY
cargo run -p kern-cli -- -p "say hi"  # bare: a model and nothing else
cargo run -p kern-cli -- --tools read,grep,glob -p "what is this project?"
cargo run -p kern-cli -- --profile claude   # a conversation with what kern reads of Claude Code
cargo run -p kern-cli -- --resume     # continue the latest session in this directory
cargo run -p kern-cli -- settings     # every setting, its value and where it came from
cargo run -p kern-cli -- models       # the models kern knows
```

## Settings

kern starts bare: no tools, no instruction files, no skills, no MCP, no mods,
no web search, in the sandbox. Every brake is on: approvals, turn caps and
conversation caps. A run gets what you turn on.

Every setting has one name, written three ways:

| Where | Spelled | Example |
|---|---|---|
| `config.toml` | snake_case | `skill_catalog = true` |
| the environment | `KERN_` + upper snake | `KERN_SKILL_CATALOG=1` |
| the command line | kebab-case | `--skill-catalog` |

The `kern` command reads these layers, each winning over the ones before it:

1. the bare defaults (`$XDG_DATA_HOME` / `$XDG_CONFIG_HOME` move their dirs)
2. `~/.config/kern/config.toml` (`$XDG_CONFIG_HOME/kern`; `KERN_CONFIG_DIR` or `--config-dir` moves it)
3. the project's `.kern/config.toml` (the nearest from here up), only inside `trusted_dirs`
4. the profile (`profile = "claude"`, `KERN_PROFILE`, `--profile`)
5. `KERN_*` variables
6. flags

`kern settings` shows each value and the layer it came from; it takes the same
flags (`kern settings --profile claude`). `kern --help` lists every flag with
its variable. A switch alone is on (`--mcp`); `--mcp=false` turns off what a
file turned on (the `=` is required, so a prompt after the flag stays the
prompt). A list is a comma list or a TOML array, plus `all` and `none`. An
unknown key or a bad value is an error that names the file, variable or flag.

```toml
# ~/.config/kern/config.toml
tools = ["read", "grep", "glob", "bash"]
context = ["agents"]
trusted_dirs = ["~/projects"]

[env]
GH_TOKEN = "${GITHUB_TOKEN}"

[profiles.review]
tools = ["read", "grep", "glob"]
```

| Switch | Bare | Turns on |
|---|---|---|
| `tools` | none | built-ins: `read`, `write`, `edit`, `bash`, `grep`, `glob`, `web_fetch`, `tasks`, `skill` (`all`) |
| `context` | none | instruction files: `agents` (`~/.agents/AGENTS.md`, then every `AGENTS.md` down to here), `claude` (`CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, `~/.claude/CLAUDE.md`) |
| `skills`, `skill_catalog` | none, false | skill dirs (`agents`, `claude`, `plugins`); the catalog lists them in the system prompt |
| `agents` | false | Agent, SendMessage, StopAgent |
| `code` | false | the Code tool (`code_tools` adds names it may call) |
| `web_search` | `off` | the provider's search: `auto`, `native`, `exa`, `parallel`, `firecrawl`, `perplexity` |
| `image` | false | GenerateImage (needs `OPENROUTER_API_KEY`) |
| `mcp` | false | the project's `.mcp.json` servers |
| `mods`, `project_mods` | false | your mods (`<config dir>/mods`), the project's (`.kern/mods`) |
| `workspace` | `sandbox` | `sandbox`: a simulated bash over a copy-on-write overlay, nothing reaches the disk until `/apply`; `real`: the real disk and bash |
| `env_passthrough` | `core` | what a process kern starts (real bash, a background job, an MCP server) gets of kern's environment: `none`, `core` (`PATH`, `HOME`, `SHELL`, `USER`, `LOGNAME`, the locale, the temp dirs), `all` (every variable, API keys included) |
| `env` | none | variables every process kern starts gets, the sandbox's shell included, over `env_passthrough`: `-E NAME=VALUE` (repeat it), `-E NAME` (kern's own value), `KERN_ENV` (one), an `[env]` table in a file |
| `approvals` | true | ask before a tool changes anything |

The caps (turn, conversation, subagents), the model, the effort and the paths
are settings too: `kern --help`, groups Limits, Subagents and Paths. So is
OpenRouter's provider routing (`provider_routing`, JSON), from your own
`config.toml`, `KERN_PROVIDER_ROUTING` or `--provider-routing`, never from a
project's file.

`env_passthrough` follows Codex's `shell_environment_policy.inherit` (same
three words, same `core` list); kern's default is `core`, so a key in your
shell stays with kern unless you pass `all`. With `none` a child gets no
`PATH`: bash is still found on the system's default path, as `execvp` does.

`env` passes a variable on purpose, the way `docker run -e` does (`-e` is
`--effort` here, so it is `-E`). The layers add to each other name by name:
your `config.toml`, a trusted project's, the profile, `KERN_ENV`, then the
flags, a later one winning for a name both set. A value fills in `${NAME}` and
`${NAME:-default}` from kern's environment, so a key can move under another
name without being written down (`GH_TOKEN = "${GITHUB_TOKEN}"`). A variable
whose `${NAME}` has no value and no default is not passed, and kern says
which. The sandbox's shell gets these variables and nothing else of your
environment. `kern settings` lists each name and where it came from, never
its value.

### Trust

A repo's own files are someone else's code. Its `.kern/config.toml`, its
`.mcp.json` (it names commands to start) and its `.kern/mods` (they run) are
read only when the working dir is inside a `trusted_dirs` entry. `trusted_dirs`
comes from your own `config.toml`, `KERN_TRUSTED_DIRS` or `--trusted-dirs`,
never from a project's file or a profile: a repo cannot trust itself.
`config_dir` comes only from the environment or a flag. Codex does the same
(`projects.<path>.trust_level`).

`.mcp.json` fills in `${NAME}` and `${NAME:-default}` from kern's environment
in a server's `command`, `args`, `env`, `url` and `headers` values, as Claude
Code does. A variable with no value and no default stays as written, and kern
says which server names it.

### The `claude` profile

`--profile claude` turns on what kern reads of Claude Code: AGENTS.md and the
CLAUDE.md family, skills from `.agents`, `.claude` and installed plugins, the
skill catalog, every built-in tool, subagents, native web search, the project's
MCP servers (in a trusted dir), the real workspace, and your whole environment
for what kern starts (`env_passthrough = all`). It is NOT parity: kern
does not read Claude Code's agents, commands, hooks, permissions, user or
plugin MCP servers, `@` imports, rules, memory or output styles.

That context costs tokens on every call. One "reply ok" turn on this machine
(2026-11-10, claude-sonnet-5.5): bare sent 179 input tokens ($0.0004),
`--profile claude` sent 45,764 ($0.11).

## Background jobs, sessions, files

Background jobs (`run_in_background` on Bash, with BashOutput and KillShell)
exist only in the real workspace; kern kills them when it exits. A job that
exits, a subagent's message for main or a background subagent's answer is a
turn you did not start: during a turn it is read at the turn's next step; at
the prompt it is announced above the line you are typing and runs when you
press Enter. `kern -p` waits for background subagents before it exits.

More providers go in `<config dir>/providers.json`, your mods in
`<config dir>/mods`; sessions are saved under `~/.local/share/kern/sessions`
(`data_dir`). In the REPL, `/help` lists the commands (`/plan` included).

## Embed it

A host builds `AgentDeps` (models, tools, system prompt, workspace, hooks),
makes an `Agent` and calls `run(input, &mut sink)` once per human turn. Every
place a host differs is a trait: `Transport` for how model calls travel, `Fs`
and `Workspace` for files and shell, `Tool` and `ReadIntercept` for tools,
`AgentHooks` for the clock, persistence and caps, `EventSink` for events,
`SessionSink` for session records, `AgentDirectory` for sub-agents.

Nothing in the library reads a file, a variable or a flag of its own accord.
A host writes its settings out:

```rust
use kern::settings::{BuiltinTools, Settings};

let settings = Settings {
    tools: BuiltinTools { read: true, bash: true, ..BuiltinTools::NONE },
    agents: true,
    ..Settings::bare("/work", "/home/me")
};
```

or opts into kern's own files, variables and flags with the `config` feature,
picking the layers and their order. Each layer wins over the ones before it;
a layer left out is not read:

```rust
use kern::config::Loader;

let loaded = Loader::new(Settings::bare(&cwd, &home)) // the defaults: bare, or the host's own
    .xdg_dirs(&env)     // the dirs under $XDG_DATA_HOME, $XDG_CONFIG_HOME
    .user_file()        // <config dir>/config.toml; .file(path) for one the host names
    .project_file()     // the project's .kern/config.toml, inside trusted_dirs only
    .profile()          // the profile `profile` names
    .env(&env)          // KERN_* variables (and the provider keys)
    .flags(&flags)      // --model, -e, ...
    .load(&fs)
    .await?;
```

That chain is `Loader::standard(defaults, &env, &flags)`, what the `kern`
command reads. `loaded` holds the settings and everything the runtime is handed
beside them. `kern::runtime::build` takes a `RuntimeInputs`: the settings, the
`World` (the real disk with the environment a child gets, or the host's own
`Fs`, where no real process starts), a `KeySource` for provider keys (a
closure, a map, a vault), the custom providers, the MCP servers and the
OpenRouter routing. What is not in there does not exist for kern. It builds a
whole conversation behind a `Ui` and an approval `Prompter` -- the CLI is that
plus a terminal. While it runs, the host changes it through the `Runtime`:
`set_model`, `set_effort`, `clear` (a fresh session file) and `rescan_skills`,
each applied to the agent, the host and the session file together. A host that
owns some parts takes the runtime's pieces, or only
`kern::tools::builtin_tools_where(|w| settings.tools.has(w), ..)`.

[`crates/kern-lib/examples/brain`](crates/kern-lib/examples/brain/) wires all of
them the way brain does, against fakes (no network, no keys), and asserts each
one. Its settings are written out in Rust, one per kind of run; no `KERN_*`
variable reaches it:

```sh
cargo run -p kern-lib --example brain
```
