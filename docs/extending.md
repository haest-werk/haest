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
(`ApprovalMod`, `PlanMod`), the host's own native mods, a `HookSet`, and mod files.
`ModLoopHooks` runs the mods' loop events after the host's own hooks.

## Every extension point

| Point | Fires | Rust | Mod file (TypeScript) | Hook (`HookSet`, any form) |
|---|---|---|---|---|
| `tool.before` | before every tool call | `Mod::tool_before` | `haest.on('tool.before', ..)` | `HookSet::on_tool_before` |
| `tool.after` | after a call ran | `Mod::tool_after` | `haest.on('tool.after', ..)` | `HookSet::on_tool_after` |
| `prompt.submit` | a human message, before the model sees it | `Mod::prompt_submit` | `haest.on('prompt.submit', ..)` | `HookSet::on_prompt_submit` |
| `system.prompt` | before every model call | `Mod::system_prompt` | `haest.on('system.prompt', ..)` | `HookSet::on_system_prompt` |
| `model.before` | before every model call, after `AgentHooks::before_model_call` | `Mod::model_before` | `haest.on('model.before', ..)` | `HookSet::on_model_before` |
| `model.after` | an answer landed, its tool calls have not run | `Mod::model_after` | `haest.on('model.after', ..)` | `HookSet::on_model_after` |
| `turn.end` | a turn is about to end on its own | `Mod::turn_end` | `haest.on('turn.end', ..)` | `HookSet::on_turn_end` |
| tools | before every model call | `Mod::tools`, or a `LiveTools` the host holds | `haest.tool({..})`, `haest.removeTool(name)` | none: a tool is called, a hook is not |
| hidden tools | before every model call | `Mod::disabled_tools` | `haest.disableTool(name)`, `haest.enableTool(name)` | none |
| slash commands | the human types `/name` | `Mod::commands` + `Mod::command` | `haest.command(name, ..)` | none |
| the host's loop | each step | `AgentHooks` | none: the host's own | none |

The gates hold the lists equal: `JsMod` and `HookSet` implement every `Mod` method
(`clippy::missing_trait_methods`), and tests drive each event through a `.ts` mod, a
`HookSet` binding, a `ShellHook` and an `HttpHook`. A new `Mod` event without its twins fails
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
`turn.end` decision already kept this turn going). Every payload also has
`session: { id, transcriptPath }`, in every form (a closure, a `ShellHook`, an `HttpHook`, a mod
file's handler): the session the conversation's `ModEnv` names as the event begins, left out
when the host names none (`transcriptPath` too). A mod file's handler gets a loop event's
payload as one object (`turn.session`) and the other events' as arguments, `session` last:
`(call, ctx, session)`, `(call, output, ctx, session)`, `(text, session)`, `(session)`;
`undefined` when none is named. [hooks.md](hooks.md) shows each event's payload, and what each
answer becomes, as JSON.

These are the mod files' own words, typed once in haest-mods (`Event` and its payload and
decision types). A bridge only encodes the payload, carries it to the hook, and decodes the
decision; the rules for applying it run in one place for every form.

One decoder reads every answer (`Event::read_decision`), a mod file's handler's included. A
key whose value is `null` counts as absent (`{ deny: null }` passes), as do `compact: false`
and a `system.prompt` answer of `false` (JavaScript's `cond && 'text'`). A value of the wrong
type (`{ stop: false }`, `{ steer: 42 }`, `{ compact: "yes" }`, a number from `prompt.submit`
or `system.prompt`) is the hook's failure: a binding's `on_error` decides what that does, and
in a mod file the event fails as a throw does. Nothing is turned into text. A mod file's handlers on one event
are asked one at a time, by the same rules as a `HookSet` binding's, in the table's last column.

## Hooks: one handler per event

`HookSet` is a mod made of bindings: a hook per event, each with

- a timeout (`DEFAULT_HOOK_TIMEOUT`, 60 s, unless `Binding::with_timeout`),
- what its failure or timeout does (`OnError::Block` or `Continue`; `block` for `tool.before`,
  `continue` for every other event),
- for `tool.before` and `tool.after`, the tools it is for (`Binding::with_matcher`: an exact
  name, or a prefix ending in `*`, the same pattern every haest setting takes; a debug build
  stops at one that is not, such as Claude Code's `Edit|Write`, which would match no tool).

A Rust closure or `async fn` taking the payload and a `CancellationToken` is a hook. So is any
type that implements `Hook<E>`, which is how the bridges plug in. The table of what `block`
does per event, and a worked example (a Bash guard with a matcher, a turn sent back once), are
on [`HookSet`](../crates/haest-mods/src/hook_set.rs).

Pick `HookSet` for a few handlers, `Mod` for something with state across events, tools or
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
[auto-compact.ts](examples/mods/auto-compact.ts) as a mod file, and its Rust twin as a closure
bound in a `HookSet` in [auto_compact.rs](../crates/haest/examples/auto_compact.rs), which runs both against a
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
default), so haest-mods and a Rust-only host link no QuickJS. Each file runs on an engine of
its own, haest-code's, the one the Code tool runs on, held to the same caps type, `VmCaps`
(`VmCaps::mod_file()`: 128 MiB, 30 s to load, 10 s of code without a pause, and 60 s for each
call, a handler, a tool or a command, to settle; the human's time on a `haest.ask` does not
count). Past a cap only that call fails, or every call in flight for a loop that ran over its
slice, and the mod runs on.

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
`bridge-http`) are hooks: bound per event in a `HookSet` as a closure is, with the binding's
timeout, `on_error` and matcher. Both are off by default. A command gets the event as JSON on
its stdin and answers on its stdout, exit 2 a blocking answer; a URL gets it as a POST and
answers in the response's body. Each speaks a dialect: `Haest` (the default, every event: the
payload above with the event's name, the decision's JSON back) or `Claude` (Claude Code's hook
JSON, so a Claude Code hook script runs as it is, on the five events Claude Code has).

[hooks.md](hooks.md) is the reference: every event's input and every answer it takes, in both
dialects, written from the code by a test that fails when they disagree; what a command and a
URL get, answer and fail on; every option of a config entry.
[`examples/hooks/no-rm-rf.sh`](examples/hooks/no-rm-rf.sh) is a Claude Code script, and
[`external_hooks.rs`](../crates/haest/examples/external_hooks.rs) runs it beside an HTTP hook
on `turn.end`.

Neither dialect holds anything of the conversation: `Haest` and `Claude` are unit values, and
the session comes with each event's payload. The host names it on the conversation's `ModEnv`
and names the new one when the human's /clear starts it; hooks bound once follow it, a
`Claude` hook as `session_id` and `transcript_path`:

```rust
let guard = ShellHook::new("sh .claude/hooks/no-rm-rf.sh").with_dialect(Claude);
let env = ModEnv::new(project, WorkspaceMode::Real, ui).with_session(SessionView::new("s-1"));
let mods = Mods::new(env, [HookSet::new().on_tool_before(Binding::new(guard).with_matcher("Bash"))]);
// The human's /clear:
mods.env().set_session(SessionView::new("s-2"));
```

Which events Claude Code has is written once, haest-mods' `ModEvent::claude_code`: the config
check, the `hook_event_name` a hook is handed, and the events a `Claude` hook compiles on all
read it.

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
```

[hooks.md](hooks.md#options) has every key (`command`, `url`, `dialect`, `matcher`,
`timeout`, `on_error`, `headers`), where entries come from (your file, a trusted project's,
the profile, `HAEST_HOOKS`, `--hooks`; a project never binds a `url`), how they run, and how a
hook moves over from Claude Code's `.claude/settings.json`.

The setting's type is haest-mods' [`HookSettings`](../crates/haest-mods/src/hook_settings.rs),
beside `Binding` and `OnError`. A Rust host builds the same entries without a file
(`HookSettings::with_entry` with a `HookEntry::shell` or `HookEntry::http`, refused what a
file is refused) and binds them with haest-bridges' `HookHost::bind`, which is what
haest-runtime does with the loaded setting.

## Which form to use

| You are | Use |
|---|---|
| a host embedding haest in Rust (brain) | `AgentHooks` for the host's own loop, native `Mod`s and a `HookSet` for the rest. No bridge, no QuickJS |
| a human customizing the CLI | mod files in `~/.config/haest/mods/` |
| a human with Claude Code hook scripts, a check in any language, or a review service, on the CLI | the [`hooks`](settings.md#hooks) setting |
| a project shipping rules with its repo | mod files in `.haest/mods/`, which the human turns on with `project_mods` in a directory they trust |
| a host with Claude Code hook scripts, or a check in any language | `ShellHook`, in the `Claude` dialect for the scripts |
| a host with a service that reviews or gates the agent | `HttpHook` |
