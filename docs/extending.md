# Extending haest

Every way to change what a haest agent does while it runs, on one page: what you can hook,
where it fires, and the forms each hook comes in. The details and the runnable examples live
in the rustdoc of the type named in each row; this page points at them.

haest is Rust first. Every extension point is a Rust trait or type, and a host like brain
uses those alone: no JavaScript engine, no child processes, one tight loop. The other forms
(a TypeScript mod file, a shell command, an HTTP endpoint) are BRIDGES onto the same Rust
point: they hand it the same payload and give back the same decision, so a hook can move
between forms without changing what it means.

## Two layers

| Layer | What | How many | A failure |
|---|---|---|---|
| [`AgentHooks`](../crates/haest-core/src/deps/hooks.rs) (haest-core) | the host's own say in the loop: persistence, compaction policy, steering, a stop hook | one per agent, set with `AgentDeps::with_hooks` | is the host's: an `Err` ends the turn |
| [`Mod`](../crates/haest-mods/src/api.rs) (haest-mods) | behaviour added on top: guards, rewrites, tools, commands, loop rules | any number, run in load order as one `Mods` | is reported to the human and never takes the conversation down; what else it means depends on the event |

A host writes `AgentHooks` once. Everything else goes in as a mod: the built-ins
(`ApprovalMod`, `PlanMod`), the host's own native mods, a set of `Hooks`, and mod files.
`ModHooks` runs the mods' loop events after the host's own hooks.

## Every extension point

| Point | Fires | Rust | Mod file (TypeScript) | Hook (`Hooks`, any form) |
|---|---|---|---|---|
| `tool.before` | before every tool call | `Mod::tool_before` | `haest.on('tool.before', ..)` | `Hooks::on_tool_before` |
| `tool.after` | after a call ran | `Mod::tool_after` | `haest.on('tool.after', ..)` | `Hooks::on_tool_after` |
| `prompt.submit` | a human message, before the model sees it | `Mod::prompt_submit` | `haest.on('prompt.submit', ..)` | `Hooks::on_prompt_submit` |
| `system.prompt` | before every model call | `Mod::system_prompt` | `haest.on('system.prompt', ..)` | `Hooks::on_system_prompt` |
| `model.before` | before every model call, after `AgentHooks::before_model_call` | `Mod::model_before` | `haest.on('model.before', ..)` | `Hooks::on_model_before` |
| `model.after` | an answer landed, its tool calls have not run | `Mod::model_after` | `haest.on('model.after', ..)` | `Hooks::on_model_after` |
| `turn.end` | a turn is about to end on its own | `Mod::turn_end` | `haest.on('turn.end', ..)` | `Hooks::on_turn_end` |
| tools | before every model call | `Mod::tools`, or a `LiveTools` the host holds | `haest.tool({..})`, `haest.removeTool(name)` | none: a tool is called, a hook is not |
| hidden tools | before every model call | `Mod::disabled_tools` | `haest.disableTool(name)`, `haest.enableTool(name)` | none |
| slash commands | the human types `/name` | `Mod::commands` + `Mod::command` | `haest.command(name, ..)` | none |
| the host's loop | each step | `AgentHooks` | none: the host's own | none |

The gates hold the lists equal: `JsMod` and `Hooks` implement every `Mod` method
(`clippy::missing_trait_methods`), and tests drive each event through a `.ts` mod, a
`Hooks` binding, a `ShellHook` and an `HttpHook`. A new `Mod` event without its twins fails
the build's gates.

## What each event may decide

| Event | Decision (Rust) | As JSON | Several hooks |
|---|---|---|---|
| `tool.before` | `Before::{Pass, Deny, Input}` | `{ deny: reason }`, `{ input: {..} }`; an answer with neither passes | rewrites chain; the first deny wins and refuses the call (the model reads the reason) |
| `tool.after` | `Option<ToolOutput>` | `{ content, isError, .. }` or nothing | each sees the output the one before left; what the call spent stays the tool's |
| `prompt.submit` | `Option<String>` | a string or nothing | rewrites chain |
| `system.prompt` | `Option<String>` | a string, or nothing (`null` or `false`) | the texts are joined |
| `model.before` | `ModelBeforeDecision { compact, steer, stop }` | `{ compact, steer, stop }` | the first stop wins; every steer is kept; the first compact counts |
| `model.after` | `ModelAfterDecision { steer, stop }` | `{ steer, stop }` | the first stop wins (the answer's calls do not run); every steer is kept |
| `turn.end` | `TurnEndDecision { compact, continue }` | `{ compact, continue }` | the first continue wins and keeps the turn going |

`compact` is `true` or `{ instructions }`: the instructions are added to haest's own summary
prompt ("keep every file path"). Each payload's JSON shape is on its type in
[payload.rs](../crates/haest-mods/src/payload.rs): `tool.before` gets
`{ call: { name, input, readOnly }, ctx: { cwd, toolCallId } }`, the loop events get
`{ modelSpec, effort, historyLength, contextTokens, contextWindow }` (`contextTokens` is what
the last answer's request held, the number to compact on; `contextWindow` is the model's
window from the host's turn caps (the CLI's: the `context_window` setting, else the catalog's),
else the catalog's, and `null` when neither knows it) plus their own fields (`model.before`: `lastMessage`;
`model.after`: `answer`; `turn.end`: `stopReason`, `error`, `answer` and `continued`, whether a
`turn.end` decision already kept this turn going).

These are the mod files' own words, typed once in haest-mods (`Event` and its payload and
decision types). A bridge only encodes the payload, carries it to the hook, and decodes the
decision; the rules for applying it run in one place for every form.

One decoder reads every answer (`Event::read_decision`), a mod file's handler's included. A
key whose value is `null` counts as absent (`{ deny: null }` passes), as do `compact: false`
and a `system.prompt` answer of `false` (JavaScript's `cond && 'text'`). A value of the wrong
type (`{ stop: false }`, `{ steer: 42 }`, `{ compact: "yes" }`, a number from `prompt.submit`
or `system.prompt`) is the hook's failure: a binding's `on_error` decides what that does, and
in a mod file the event fails as a throw does. Nothing is turned into text. A mod file's handlers on one event
are asked one at a time, by the same rules as a `Hooks` binding's, in the table's last column.

## Hooks: one handler per event

`Hooks` is a mod made of bindings: a hook per event, each with

- a timeout (`DEFAULT_HOOK_TIMEOUT`, 60 s, unless `Binding::with_timeout`),
- what its failure or timeout does (`OnError::Block` or `Continue`; `block` for `tool.before`,
  `continue` for every other event),
- for `tool.before` and `tool.after`, the tools it is for (`Binding::with_matcher`: an exact
  name, or a prefix ending in `*`, the same pattern every haest setting takes).

A Rust closure or `async fn` taking the payload and a `CancellationToken` is a hook. So is any
type that implements `Hook<E>`, which is how the bridges plug in. The table of what `block`
does per event, and a worked example (a Bash guard with a matcher, a turn sent back once), are
on [`Hooks`](../crates/haest-mods/src/hook_set.rs).

Pick `Hooks` for a few handlers, `Mod` for something with state across events, tools or
commands.

## Tools that change while the conversation runs

Two ways, by who owns the tools:

- **The host's own tools**: hand the agent a [`LiveTools`](../crates/haest-core/src/tool/live.rs)
  (a `ToolSource`) and keep a clone. `set_tool` adds or replaces one, `remove_tool` drops one,
  `set_enabled` hides one and brings it back in its place, `set_tools` reloads the set. The loop
  reads the set before each model call; a call already running finishes on the tool it started
  with.
- **A mod's tools**: `Mod::tools` is asked again before every model call, so a mod that
  returns a new list has a new set from the next call. A mod hides any tool, whoever added it,
  with `Mod::disabled_tools`; a name it stops returning comes back as it was. Removing and
  disabling are different promises: a removed tool is gone, a disabled one returns unchanged.

Rust mods load once and never reload. What changes lives inside a mod: its own `LiveTools`, or
the mod files a file-loading mod holds.

The prompt cache: tools come first in a request. A replacement the model cannot tell from the
old tool (same name, description, schema) costs nothing anywhere. Any other change (a tool
added, removed, hidden or shown, or a new description or schema) depends on the model:

- On a model whose catalog entry has `cache.toolChanges` (Anthropic's own API for Claude
  Opus 5.5 and Sonnet 5.5 in the bundled catalog), it costs no miss. The request keeps the
  `tools` it first sent, byte for byte, and the loop records what changed as a
  `Message::ToolChange` in history, sent as a `system` message of `tool_addition` /
  `tool_removal` blocks (beta `inline-tools-2026-09-15`) right after the next user message or
  tool results. A change made after a turn ended waits for the next user message; changes
  between two model calls go out as one. The session file keeps them, so a resumed
  conversation sends the same bytes. Compaction drops them and starts over from the tools on
  offer then.
- On any other model (OpenRouter's Anthropic models included, which drop these blocks), the
  request is sent the current tools, and the next model call pays one cache miss.

On the other models, a host that changes tools often defers them (`DeferMode::Proxy`); a
deferred tool's schema can then change for free, but adding or removing one still changes
ToolSearch's list and costs the miss.

## Compaction from an extension

| From | How |
|---|---|
| the host | `AgentHooks::before_model_call` calls `haest_core::compaction::compact` on the agent it is handed |
| a Rust mod | `Mod::model_before` or `Mod::turn_end` gets `&mut Agent` and the turn's cancel token: call `compact` with `CompactionOptions::instructions` for its own focus |
| a mod file or any hook | answer `{ compact: true }` or `{ compact: { instructions } }` from `model.before` or `turn.end` |

Every compaction, whoever ran it, reaches `AgentHooks::on_compaction`, so a host that saves
history records it. What the summary call costs counts toward the turn's caps and the
conversation's ledger. `turn.end` is the place to compact so the next turn starts small; never
compact in `model.after` (the answer's tool results land after it).

Compacting past a share of the window, in both forms:
[auto-compact.ts](examples/mods/auto-compact.ts) as a mod file, and its Rust twin as a `Hooks`
closure in [auto_compact.rs](../crates/haest/examples/auto_compact.rs), which runs both against a
scripted model (`cargo run -p haest --example auto_compact`). Compacting on the cache's clock
instead (before an idle conversation's cache expires) is `compact_before_expire`, below.

## The prompt cache

[`CachePolicy`](../crates/haest-core/src/cache/policy.rs) is a plug point with two questions:

| Method | Asked | Answers |
|---|---|---|
| `plan` | for every request (the loop's, a pre-warm, a summary) | a `CachePlan`: which blocks carry a breakpoint (system, tools, history, chosen messages) and for `5m` or `1h` |
| `on_idle` | at each idle tick, a little before the cache would expire | an `IdleAction`: nothing, ping (keep the cache warm), or compact while it is still warm |

haest checks every plan before it is sent (at most 4 breakpoints, a longer TTL before a shorter
one, nothing below the model's minimum cacheable length) and fixes and notes a plan that breaks
a rule rather than sending it to fail. `IdleState` hands `on_idle` the prices and the cache
facts it needs to decide (`compaction_pays`), so a policy never pays more to keep a cache than
the cache saves.

A Rust host sets its own with `AgentOptions::with_cache_policy` or
`Agent::set_cache_policy`. The CLI's policy is the [`[cache]`](settings.md#cache) setting:
breakpoints and TTLs per block, `keep_warm`, `compact_before_expire`, per model under
`[cache.models."<spec>"]`. The idle ticks run where a conversation stays open between turns:
haest-session's driver calls `Agent::next_cache_tick` and `Agent::cache_tick`.

## Mod files

A `.js`, `.mjs` or `.ts` file whose default export receives the `haest` API, run in embedded
QuickJS (TypeScript types are stripped in Rust; no node, no bun). Mod files are the first
bridge: they live in haest-bridges behind its `js` feature (the facade's `bridge-js`, on by
default), so haest-mods and a Rust-only host link no QuickJS.

[`ModFiles`](../crates/haest-bridges/src/js/files.rs) is a conversation's mod files as ONE
mod, added after the built-ins and before the host's own mods. haest-runtime loads:

1. the human's own: `<config_dir>/mods/*`, with the `mods` setting,
2. the project's: `<cwd>/.haest/mods/*`, ONLY with `project_mods`. A project's mod is code
   from that repo running with the host's full trust, so cloning a repo is never enough to
   run it.

A mod file can do everything in the table above, plus ask the human (`haest.ask`, an Allow or
Deny prompt) and show them a line (`haest.notice`). The whole API is on
[`JsMod`](../crates/haest-bridges/src/js/mod.rs); runnable examples are in the haest-bridges
crate docs.

### Reloading mod files

The [`mod_reload`](settings.md#mod_reload) setting (`off` by default, your own config only,
never a project's) has `ModFiles` read every file again before each model call. No watcher:
a change lands at the next step boundary.

| On disk | `auto` | `ask` |
|---|---|---|
| a new or changed file | loads | loads on the human's Allow |
| a deleted file | unloads, its tools leave the next request | the same, on the human's Allow |
| a file that fails to load | the version before keeps running; reported once | the same |

`ask` exists because in the real workspace the agent can write a mod file, and a mod runs with
the host's trust: without it, the agent's own file would rewrite the human's next message one
step later. A Deny keeps the running version and is not asked again for that content (or that
deletion). The files swap as one `Mods`, so the next request has the new file's tools and
hidden tools. Subagents see the swapped files but never trigger a reload themselves. Rust mods
never reload.

Every mod, native or file, runs with the host's full trust, sandbox or not.

## Hooks in another process

[`ShellHook`](../crates/haest-bridges/src/shell.rs) (haest-bridges feature `shell`, the
facade's `bridge-shell`) and [`HttpHook`](../crates/haest-bridges/src/http.rs) (`http`,
`bridge-http`) are hooks: bound per event in `Hooks` as a closure is, with the binding's
timeout, `on_error` and matcher. Both are off by default.

| | `ShellHook` | `HttpHook` |
|---|---|---|
| per event | a command, `bash -c`, on the host (never in the sandbox) | a POST, through haest-http's `HttpClient` |
| the input | on stdin | the body, `content-type: application/json` |
| the decision | stdout on exit 0; empty passes | a 2xx response's body; empty passes |
| a blocking answer | exit 2, stderr its text | the decision itself |
| a failure (`on_error`) | any other exit, or stdout that is no decision | another status, no answer, or a body that is no decision |
| at the timeout or an interrupt | its whole process group is killed | the request is dropped |
| it gets | the variables the host hands in, nothing else of its environment | the headers the host sets |

Exit 2 refuses a `tool.before` call, stops the turn on `model.before` and `model.after`, and
keeps a `turn.end` going; on the other events it is a failure. haest fails closed: a broken
`tool.before` hook refuses the call.

Each speaks a dialect:

| Dialect | The input | The answer | Events |
|---|---|---|---|
| `Haest`, the default | `{ "event": "<name>", ...payload }`: the payload above, with the event's name | the decision's JSON above | every one |
| `Claude` | Claude Code's hook JSON (`hook_event_name`, `tool_name`, `tool_input`, `stop_hook_active`, ..), plus `CLAUDE_PROJECT_DIR` for a command | Claude Code's (`permissionDecision`, `updatedInput`, `updatedToolOutput`, `additionalContext`, `decision: "block"`) | `tool.before` (PreToolUse), `tool.after` (PostToolUse), `prompt.submit` (UserPromptSubmit), `system.prompt` (SessionStart), `turn.end` (Stop) |

So a Claude Code hook script runs as it is. Where Claude Code can do more than haest's event
(an `ask`, refusing a prompt, `continue: false`), haest refuses or ignores, and a `Claude` hook
on `model.before` or `model.after` does not compile; the mapping and that list are on
[`Claude`](../crates/haest-bridges/src/dialect/claude.rs) and in
[haest-bridges' README](../crates/haest-bridges/README.md#dialects).
[`examples/hooks/no-rm-rf.sh`](examples/hooks/no-rm-rf.sh) is such a script, and
[`external_hooks.rs`](../crates/haest/examples/external_hooks.rs) runs it beside an HTTP hook
on `turn.end`.

A hook runs with the host's full trust: it receives every tool input, and a command runs on
the host whatever the workspace. Load a project's hooks only in a directory the human trusts,
and an HTTP hook only from the human's own configuration. A host binds them in Rust as above,
or the human writes them in a config file.

### Hooks from a config file

The [`hooks`](settings.md#hooks) setting binds the same `ShellHook`s and `HttpHook`s with no
Rust: one `[[hooks."<event>"]]` entry each, under the protocol's own event name, quoted for
its dot (`[[hooks.tool.before]]` is a table `tool` to TOML, which haest refuses with a hint).

```toml
# A Claude Code PreToolUse script, as it is, on Bash calls.
[[hooks."tool.before"]]
command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
dialect = "claude"
matcher = "Bash"

# A reviewer that may keep a turn going, in haest's own JSON.
[[hooks."turn.end"]]
url = "https://review.example/haest/turn-end"
headers = { authorization = "Bearer ${REVIEW_TOKEN}" }
timeout = 10
```

| Key | Takes | Default |
|---|---|---|
| `command` | a command, `bash -c` on the host per event, in the working dir: a `ShellHook` | |
| `url` | an `http://` or `https://` URL, POSTed per event: an `HttpHook` | |
| `dialect` | `haest` (every event) or `claude` (Claude Code's JSON, on the events Claude Code has: never `model.before` or `model.after`) | `haest` |
| `matcher` | on `tool.before` and `tool.after` only: a tool's name (`Bash`) or a prefix ending in `*` (`mcp__github__*`). Claude Code's `Edit\|Write` is a regular expression, which haest does not read: write one entry per tool | every tool |
| `timeout` | seconds (`10`, `2.5`); past them the hook failed | 60 |
| `on_error` | what a failure does: `block` or `continue` | `block` on `tool.before`, else `continue` |
| `headers` | a `url`'s headers, `{ name = "value" }`; `${NAME}` and `${NAME:-default}` in a value are filled in from haest's environment | none |

An entry takes `command` or `url`, never both. Loading refuses an unknown event or key, an
entry with neither or both, `dialect = "claude"` on `model.before` or `model.after`, a
`matcher` on an event that is not about a tool, and `headers` on a command. The error names
the file and the entry as that file numbers it
(``~/.config/haest/config.toml: hooks: [[hooks."turn.end"]] #2: unknown key `comand` ``),
and never a header's value.

Where they come from:

- Every layer's entries bind after the ones before it, in the order written: your file, a
  trusted project's file, the profile, `HAEST_HOOKS`, then each `--hooks`. A variable or a
  flag holds the same as TOML: `--hooks '{ "turn.end" = [{ command = "sh check.sh" }] }'`.
- A project's `.haest/config.toml` is read only inside your
  [`trusted_dirs`](settings.md#trusted_dirs), so an untrusted repo's hooks never load. A
  trusted one may bind a `command` (it already runs its own code through its mods and its
  `.mcp.json`), never a `url`: a POST sends the conversation's events where the repo says. A
  `url` comes from your own file, a variable or a flag; a project's file, or a profile it
  defines, that holds one fails to load.
- Off by default: no entry, no hook.

How they run:

- The entries are one mod, `[hooks]` in reports (`Mod [hooks] failed in tool.before: hook 2
  on tool.before failed: ..`, where `hook 2` is the event's second bound entry across the
  layers), after the built-in mods and your mod files, before a host's own mods.
- A command runs on the host in the working dir, with the environment every process haest
  starts gets ([`env_passthrough`](settings.md#env_passthrough), [`env`](settings.md#env)):
  `~` in a command needs `HOME`, which `core` passes. Give a command in your own file as an
  absolute path or under `~/`: a relative one runs whatever the working dir has at that path.
  A host whose world starts no process (haest-runtime's `World::files`) binds no command: it
  warns, and binds the rest.
- A URL goes through the runtime's `HttpClient`, the one the model calls use.
- In the `claude` dialect a hook gets the session's id and its file as Claude Code's
  `session_id` and `transcript_path`, as of that event: after a /clear, the new session's.
- A subagent's tool calls pass through the hooks, and its system prompt takes what
  `system.prompt` adds, as with every mod. `prompt.submit`, `model.before`, `model.after` and
  `turn.end` are the main conversation's: a subagent's turn is not the human's, and a check
  that keeps the main turn going would hold every subagent's to it too.
- `haest settings` shows a header by its name only.

A hook from Claude Code's `.claude/settings.json` moves over one entry at a time: the event by
its haest name (`PreToolUse` is `tool.before`, `PostToolUse` `tool.after`, `UserPromptSubmit`
`prompt.submit`, `SessionStart` `system.prompt`, `Stop` `turn.end`), its `command` and
`timeout`, its `matcher` (one entry per tool), and `dialect = "claude"`.

## Which form to use

| You are | Use |
|---|---|
| a host embedding haest in Rust (brain) | `AgentHooks` for the host's own loop, native `Mod`s and `Hooks` for the rest. No bridge, no QuickJS |
| a human customizing the CLI | mod files in `~/.config/haest/mods/` |
| a human with Claude Code hook scripts, a check in any language, or a review service, on the CLI | the [`hooks`](settings.md#hooks) setting |
| a project shipping rules with its repo | mod files in `.haest/mods/`, which the human turns on with `project_mods` in a directory they trust |
| a host with Claude Code hook scripts, or a check in any language | `ShellHook`, in the `Claude` dialect for the scripts |
| a host with a service that reviews or gates the agent | `HttpHook` |
