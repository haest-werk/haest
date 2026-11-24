# haest

One Rust agent loop and tool harness. A model call, the tools it asks for, the
results fed back, until the model answers without tools. Two hosts embed it:
the `haest` CLI and brain.

New here: [docs/quickstart.md](https://github.com/haest-werk/haest/blob/main/docs/quickstart.md)
(the command and an embedded host, five minutes each), then
[docs/howto.md](https://github.com/haest-werk/haest/blob/main/docs/howto.md) (one recipe per
task). Decisions and status live in
[PLAN.md](https://github.com/haest-werk/haest/blob/main/PLAN.md), what is not built yet in
[ROADMAP.md](https://github.com/haest-werk/haest/blob/main/ROADMAP.md), working rules in
[CONTRIBUTING.md](https://github.com/haest-werk/haest/blob/main/CONTRIBUTING.md),
what each release changed in
[CHANGELOG.md](https://github.com/haest-werk/haest/blob/main/CHANGELOG.md).

On crates.io as [`haest`](https://crates.io/crates/haest) (docs on
[docs.rs](https://docs.rs/haest/)), source at
[github.com/haest-werk/haest](https://github.com/haest-werk/haest/).

## Crates

| Crate | What |
|---|---|
| `haest-core` | the loop and the contracts: messages, `Provider`, `Tool`, `Fs`, `Workspace`, `AgentHooks`, events; turn guard, caps, ledger, thresholds (a host's own beside the guard's nudge, the ledger's warning, compaction and wind-down, opt-in), session log, compaction, task board |
| `haest-http` | the HTTP seam (`HttpClient`) the model calls, GenerateImage and the GitHub tracker send through, and its default over reqwest and rustls; the one place haest builds a reqwest client. WebFetch sends through a guarded client of its own, MCP servers through rmcp's |
| `haest-providers` | Anthropic Messages, OpenAI Chat Completions (any compatible server, OpenRouter), OpenAI Responses; `providers.json`; the model catalog; requests over any `HttpClient` |
| `haest-workspace` | the real disk and bash, or a bashkit sandbox over a copy-on-write overlay (or over any `Fs`) whose edits are a change set the human reviews and applies |
| `haest-tools` | Read, Write, Edit, Glob, Grep, Bash (+ background jobs), WebFetch, tasks, ToolSearch, Skill, Agent / SendMessage / StopAgent, GenerateImage |
| `haest-mcp` | MCP client: stdio and streamable HTTP servers, their tools as haest tools |
| `haest-code` | embedded QuickJS, the one engine all of haest's JavaScript runs on: the Code tool, the sandbox `js` command, and (through `haest-bridges`) mod files |
| `haest-mods` | mods: change how haest behaves without forking it, in Rust: native mods, hooks bound per event, the events' protocol (payloads and decisions, typed once), approvals and plan mode. Links no script engine |
| `haest-bridges` | mods and hooks written outside Rust, speaking haest-mods' protocol, one feature per bridge: `js` runs `.js`/`.ts` mod files in embedded QuickJS and reloads a conversation's mod files as they change (`mod_reload`); `shell` runs a command per event (`ShellHook`), `http` POSTs each event to a URL (`HttpHook`), both in haest's JSON or Claude Code's hook JSON, so Claude Code hook scripts run as they are |
| `haest-ts` | the one TypeScript stripper Code, `js` and mod files share |
| `haest-settings` | the settings as plain typed values (`Settings::bare`), no files, no environment, no flags |
| `haest-config` | the opt-in loader, a builder (`Loader`): TOML files, `HAEST_*` variables and flags in the order a host adds them, profiles, trust; plus what the runtime is handed beside the settings (keys, `providers.json`, `.mcp.json`, a child's environment) |
| `haest-runtime` | a whole conversation built from what a host hands it, behind its `Ui`; what the CLI is made of |
| `haest-prompt` | opt-in: the system prompt as the host's Jinja template, rendered by MiniJinja the way Hugging Face chat templates are; it sees haest's built sections, the model's catalog row (`family`, `tier`, `generation`), the tools and the host's own values |
| `haest-wire` | the wire format another program reads and drives a conversation with: the envelope around its events (`v`, `sid`, `seq`, `ts`, `path`), the requests a peer sends, a replay ring with gaps, an NDJSON codec; serde only |
| `haest-session` | a conversation another program watches and drives while it runs: the session driver (one turn at a time, input mid-turn steered in, the rest queued through the turn gate), its handle (`Backend`), a numbered stream every subscriber reads with catch-up and one `gap` when it falls behind, approval asks answered over it |
| `haest-transport` | the session served to another program: one connection's rules (`hello`, a reply per request, the stream as asked for, the access check) over any line source and sink, and `Peer`, the asking side; `Stdio` for a parent process (feature `stdio`), `UnixSocket` for programs on this machine (feature `unix`), `Broker` to dial out to a broker (feature `broker`) |
| `haest-acp` | haest as an Agent Client Protocol agent: an editor (Zed's agent panel, JetBrains, any ACP client) drives whole conversations over stdin and stdout, its prompts as turns, tool calls with their kinds, files and diffs, approval asks as permission requests, plan mode as a mode; the host says how a conversation is built for a directory (`Conversations`). `haest acp` serves it |
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
| `http` | `haest::http` (the HTTP seam: `HttpClient`, how every model request travels; not WebFetch's or MCP's clients); on with `providers` and `tools` | yes |
| `reqwest` | the default `HttpClient`, `haest::http::ReqwestClient`, over reqwest and rustls. Off, a host hands the providers its own client; `tools` and `mcp` build reqwest anyway (WebFetch and rmcp run on it) | yes |
| `providers` | `haest::providers` | yes |
| `workspace` | `haest::workspace` | yes |
| `tools` | `haest::tools` | yes |
| `mcp` | `haest::mcp` | yes |
| `rmcp` | `haest::mcp::rmcp` and `McpHub::connect_transport` (an MCP server over a transport the host opened; rmcp's types in haest's API); turns on `mcp` | no |
| `code` | `haest::code` (the Code tool, the sandbox `js` command) | yes |
| `mods` | `haest::mods` (native mods and hooks, approvals, plan mode; no script engine) | yes |
| `bridge-js` | `haest::bridges` (`.js`/`.ts` mod files in embedded QuickJS, reloaded as they change); turns on `mods` | yes |
| `bridge-shell` | `haest::bridges::ShellHook`: a hook that is a command run on the host per event, JSON on its stdin and stdout, in haest's dialect or Claude Code's; turns on `mods` | no |
| `bridge-http` | `haest::bridges::HttpHook`: a hook behind a URL, a POST per event through `haest::http`, in the same two dialects; turns on `mods` and `http` | no |
| `settings` | `haest::settings` (the values, plain) | yes |
| `config` | `haest::config` (the loader: files, variables, flags, profiles, trust) | no |
| `runtime` | `haest::runtime` (the conversation built from what it is handed) and everything above but `config` | no |
| `testing` | `haest::core::testing` (a scripted model that records every request, for a host's tests); with `providers` `haest::providers::testing` (a fake HTTP client, SSE bodies), with `http` `haest::http::testing::Server` (an HTTP server on 127.0.0.1). For `dev-dependencies` only: every `testing` module and feature in haest is outside semver and changes in any release | no |
| `wire` | `haest::wire` (the envelope, requests, replay ring, NDJSON codec) | no |
| `session` | `haest::session` (the session driver, its handle and stream); with `runtime`, the whole conversation as one | no |
| `transport` | `haest::transport` (the session served to another program, over any line source and sink) | no |
| `transport-stdio` | `haest::transport::Stdio` (a parent process drives the conversation over stdin and stdout) | no |
| `transport-unix` | `haest::transport::UnixSocket` (programs on this machine watch or drive the conversation over a Unix socket) | no |
| `transport-broker` | `haest::transport::Broker` (the conversation dials out to a broker over a WebSocket; pulls a WebSocket client on rustls) | no |
| `prompt` | `haest::prompt` (the system prompt as the host's Jinja template); with `runtime`, `RuntimeInputs::with_prompt_template` renders it before every model call | no |
| `acp` | `haest::acp` (haest as an Agent Client Protocol agent: `AcpAgent` serves an editor over stdin and stdout); turns on `runtime` and `session`. The protocol's crate turns on serde_json's `preserve_order` for the whole build: every `serde_json::Map`, yours too, keeps keys in insertion order, not sorted | no |

```toml
[dependencies]
haest = "0.5"
# or, the whole conversation with haest's own files, variables and flags (the CLI's way):
# haest = { version = "0.5", features = ["runtime", "config"] }
# or, a host with its own HTTP client, files and tools (the notebook example);
# `tools` still builds reqwest for WebFetch, so drop it too for a tree with none:
# haest = { version = "0.5", default-features = false, features = ["providers", "workspace", "tools", "settings"] }
```

## Run the CLI

```sh
cargo install haest-cli               # puts `haest` on your PATH (macOS, Linux: Unix only)
export OPENROUTER_API_KEY=...         # openrouter: models; anthropic: takes ANTHROPIC_API_KEY, openai: OPENAI_API_KEY
haest -p "say hi"                     # bare: a model and nothing else
haest --tools read,grep,glob -p "what is this project?"
haest --profile claude                # a conversation with what haest reads of Claude Code
haest --resume                        # continue the latest session in this directory
haest --stdio                         # driven by a parent process: wire requests on stdin, frames on stdout
haest ctl events <id> --follow        # watch a haest running with control_socket = "watch" (haest ctl list finds it)
haest --broker-url wss://broker.example/agents   # a broker watches the conversation; its token is HAEST_BROKER_TOKEN
haest acp --profile claude            # an editor's agent (Zed, JetBrains): docs/howto.md, "Use haest from Zed (ACP)"
haest settings                        # every setting, its value and where it came from
haest models                          # the models haest knows
```

From a checkout, `cargo run -p haest-cli --` takes the same arguments. Every
provider's key and endpoint variable is in
[docs/environment.md](https://github.com/haest-werk/haest/blob/main/docs/environment.md#provider-keys).

## Settings

haest starts bare: no tools, no instruction files, no skills, no MCP, no mods,
no web search, in the sandbox. Every brake is on: approvals, turn caps and
conversation caps. A run gets what you turn on. New behaviour is opt-in too:
`wind_down_at = 0.8` stops new subagents at 80% of a cap and tells the model
once to conclude; it is off unless set, the `claude` profile included.

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
| [docs/quickstart.md](https://github.com/haest-werk/haest/blob/main/docs/quickstart.md) | the command (a key, tools, a hook script, a mod file) and an embedded host (one turn, a hook), five minutes each |
| [docs/howto.md](https://github.com/haest-werk/haest/blob/main/docs/howto.md) | one recipe per task, as a config entry, a mod file and Rust: guard a tool, keep a turn going, a mod file's tool, reload mod files, tools that change, compaction, the prompt cache, a URL hook |
| [docs/settings.md](https://github.com/haest-werk/haest/blob/main/docs/settings.md) | every setting: what it does, what it takes, its default, where it may be set, and an example in all three spellings; the built-in profiles. Written from the registry, so it is always the code's |
| [docs/environment.md](https://github.com/haest-werk/haest/blob/main/docs/environment.md) | the variables haest reads (`HAEST_*`, `HOME`, the XDG dirs, provider keys), what a process haest starts gets (`env_passthrough`), passing a variable on purpose (`-E`, `[env]`), `${NAME}` in config files, recipes |
| [docs/extending.md](https://github.com/haest-werk/haest/blob/main/docs/extending.md) | every way to change the agent while it runs: the host's `AgentHooks`, mods and their events, hooks bound per event in a `HookSet`, tools that change (`LiveTools`), compaction from a hook (an auto-compact mod file and its Rust twin), the prompt cache policy, mod files and their reloading, hooks in another process (`ShellHook`, `HttpHook`); which form to use |
| [docs/examples/](https://github.com/haest-werk/haest/tree/main/docs/examples/) | every runnable example, one line each, and the files: a whole `config.toml`, a project's `.haest/config.toml`, `.mcp.json`, `providers.json`, `HAEST_*` exports, a Claude Code hook script, a mod file; each run or loaded by a test |

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
plugin MCP servers, `@` imports, rules, memory or output styles. Claude
Code's hook scripts do run as they are when you bind them with the `hooks`
setting (`dialect = "claude"`).

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

```toml
[dependencies]
haest = "0.5"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

One turn with a real model and haest's built-in tools, in the current
directory:

<!-- The same code as the Quickstart in crates/haest/src/lib.rs, where it compiles. -->

```rust
use haest::providers::ProviderRegistry;
use haest::tools::{BuiltinOptions, builtin_tools};
use haest::workspace::RealWorkspace;
use haest::{Agent, AgentDeps, AgentOptions, BoxError, ChildEnv, LoopEvent, SharedDeps};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    // The host decides where keys come from; here, this process's environment.
    let keys = |name: &str| std::env::var(name).ok();
    // The built-in providers; `.with_custom(..)` adds providers.json's.
    let models = ProviderRegistry::new(keys);

    // Bash runs with only the variables you pass on.
    let pass = |k: &'static str| (k, std::env::var(k).unwrap_or_default());
    let env = ChildEnv::new([pass("PATH"), pass("HOME")]);

    // What the conversation runs on: models, a working directory, files and shell.
    let shared = SharedDeps::new(models, std::env::current_dir()?, RealWorkspace::new(env));
    // The agent's own: its tools and its system prompt.
    let tools = builtin_tools(BuiltinOptions::default());
    let deps = AgentDeps::new(shared, tools, "You are a careful engineer.");
    let model = AgentOptions::new("openrouter:anthropic/claude-sonnet-5.5");
    let mut agent = Agent::new(deps, model);

    // Every event of the turn goes to the sink, in order.
    // This one prints the answer as it streams.
    let mut sink = |ev: LoopEvent| match ev {
        LoopEvent::TextDelta { text } => print!("{text}"),
        LoopEvent::ToolCallStart { name, .. } => println!("\n[{name}]"),
        _ => {}
    };
    let end = agent.run("What does this project do?", &mut sink).await;
    println!("\n{:?}", end.stop_reason);
    Ok(())
}
```

Run it with `OPENROUTER_API_KEY` set. `anthropic:` and `openai:` models take
`ANTHROPIC_API_KEY` and `OPENAI_API_KEY`; a `providers.json` adds any
OpenAI-compatible server.

A host builds `AgentDeps` (the agent's tools and system prompt, and the
`SharedDeps` every agent of the conversation shares: models, workspace, hooks
and the rest), makes an `Agent` and calls `run(input, &mut sink)` once per
human turn. Each part goes in as the value itself (a closure, a string, a
host's own type), with no `Arc` around it; a part the host already shares
goes in as its `Arc`, so one object can fill several slots.

Every place a host differs is a trait: `HttpClient` for how requests
travel, `Fs` and `Workspace` for files and shell, `Tool` and
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
    .env(env)           // HAEST_* variables (and the provider keys), as pairs or a map
    .flags(flags)       // --model, -e, ...
    .load(&fs)
    .await?;
```

That chain is `Loader::standard(defaults, env, flags)`, what the `haest`
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
file together. A host that owns some parts builds its own `AgentDeps` instead,
with `haest::tools::builtin_tools_where(|t| settings.tools.has(t.as_str()), ..)`
for haest's tools.

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

## Limitations

haest leaves these cases alone on purpose: handling each would cost more
code than the case is worth. Where a file is at stake, the line says what
still keeps it safe.

The sandbox and its apply:

- On a disk that reads two names as one (macOS's default ignores case and
  Unicode form; so do exFAT, FAT, Linux vfat and case-folding directories),
  a rename by case or Unicode form alone (`mv README.md readme.md`), or a
  new file beside one the disk reads as the same name, is held as a
  conflict when applied. Nothing is overwritten; rename it on disk. The
  change list does not ask the disk, so it shows such a change as no
  conflict until an apply skips it.
- A swap through a temporary name (`mv a t && mv b a && mv t b`) is applied
  in path order. If the disk refuses its second write, the first has
  already gone over its file; that file's old text is then only in the
  sandbox, which keeps the change listed.
- A 0700 directory the sandbox changed something under before moving or
  copying it (`echo x > certs/new && mv certs keys`) lands at the umask's
  mode (0755). A moved or copied file keeps its mode.
- A save that lands between the apply's last look at a file and its
  replace is lost. The window is the time it takes to write that one file;
  Edit and Write in the real workspace have the same window.
- Writing over a file (the apply, and Edit and Write in the real
  workspace) renames a new file onto it. The new file keeps the mode,
  owner and group, not extended attributes or ACLs, and whatever holds the
  old file keeps the old one: a process that has it open (a log a server
  appends to, `tail -f`), an inotify watch, a container it is bind-mounted
  into on its own. A file whose owner or group the run may not give
  (another user's, a group the run is not in), or whose mode the disk does
  not keep, is not written: the write fails and the file stays as it was.
- A file with a second name (a hard link), a symlink, a FIFO or a device
  is written in place, so a full disk can cut it short; the write says so.
  Where the sandbox deletes one name of a hard-linked file and edits
  another (`rm a && echo y >> b`), the edit changes the deleted name too,
  so that delete is held as a conflict. Nothing is removed.
- A file written over by a rename survives a kill (the old file or the
  whole new one stays), but is not synced before the rename: after a power cut, a disk that does
  not flush data ahead of a rename (XFS) can hold an empty file.
- The overlay lives in the process: edits not applied before it exits are
  gone, and a resumed session starts from a clean overlay.

Sessions:

- Only the real disk locks a session file (`flock`). Over `MemFs`, or a
  host `Fs` without `try_lock`, two runs resumed on one session both write
  it.

Code a host plugs in is trusted to keep its contract; haest does not
guard against it:

- A panic in a host's backend, line source or line sink ends a transport
  connection at once and reaches the host. The peer sees the connection
  close with no reply to that request, and catches up with
  `events {after}` when it connects again.
- A `JobEvents` handler must not panic. One that panics on a job's output
  loses the rest of that output and the job is reported ended with exit
  code -1; the job table stays right, so BashOutput and KillShell still
  read it.
- A `Threshold`'s `Drop` must not panic: it can run while the agent holds
  its threshold list, and every later turn of that agent would panic too.
  No conversation data is lost.
- `LineSink::keepalive` must wait for a reason to send. One that returns
  at once takes the writer over and the connection hangs; nothing is lost
  or half sent.

Elsewhere:

- Unix only (macOS, Linux).
- The `claude` profile reads part of Claude Code's setup, not all of it
  (see [The `claude` profile](#the-claude-profile)).
- A shell or HTTP hook in Claude Code's dialect runs Claude Code's hook
  scripts as they are, but haest's events take less than Claude Code's: an
  `ask` refuses the call, a prompt cannot be refused, `continue: false` is
  ignored, and matchers are haest's, not regular expressions. The list is in
  [haest-bridges' README](crates/haest-bridges/README.md#dialects).

## License

MIT, see [LICENSE](https://github.com/haest-werk/haest/blob/main/LICENSE).
