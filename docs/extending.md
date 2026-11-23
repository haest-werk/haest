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
(`clippy::missing_trait_methods`), and tests drive each event through a `.ts` mod and a
`Hooks` binding. A new `Mod` event without its twins fails the build's gates.

## What each event may decide

| Event | Decision (Rust) | As JSON | Several hooks |
|---|---|---|---|
| `tool.before` | `Before::{Pass, Deny, Input}` | `{ deny: reason }`, `{ input: {..} }`, anything else passes | rewrites chain; the first deny wins and refuses the call (the model reads the reason) |
| `tool.after` | `Option<ToolOutput>` | `{ content, isError, .. }` or nothing | each sees the output the one before left |
| `prompt.submit` | `Option<String>` | a string or nothing | rewrites chain |
| `system.prompt` | `Option<String>` | a string or nothing | the texts are joined |
| `model.before` | `ModelBeforeDecision { compact, steer, stop }` | `{ compact, steer, stop }` | the first stop wins; every steer is kept; the first compact counts |
| `model.after` | `ModelAfterDecision { steer, stop }` | `{ steer, stop }` | the first stop wins (the answer's calls do not run); every steer is kept |
| `turn.end` | `TurnEndDecision { compact, continue }` | `{ compact, continue }` | the first continue wins and keeps the turn going |

`compact` is `true` or `{ instructions }`: the instructions are added to haest's own summary
prompt ("keep every file path"). Each payload's JSON shape is on its type in
[payload.rs](../crates/haest-mods/src/payload.rs): `tool.before` gets
`{ call: { name, input, readOnly }, ctx: { cwd, toolCallId } }`, the loop events get
`{ modelSpec, effort, historyLength, .. }` plus their own field (`lastMessage`, `answer`,
`stopReason` and `error`).

These are the mod files' own words, typed once in haest-mods (`Event` and its payload and
decision types). A bridge only encodes the payload, carries it to the hook, and decodes the
decision; the rules for applying it run in one place for every form.

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

The prompt cache: tools come first in a request, so a tool added, removed, hidden or shown,
or a new description or schema, costs the next model call one cache miss. A replacement the
model cannot tell from the old tool (same name, description, schema) costs nothing. A host
that changes tools often defers them (`DeferMode::Proxy`); a deferred tool's schema can then
change for free, but adding or removing one still changes ToolSearch's list and costs the miss.

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

## Which form to use

| You are | Use |
|---|---|
| a host embedding haest in Rust (brain) | `AgentHooks` for the host's own loop, native `Mod`s and `Hooks` for the rest. No bridge, no QuickJS |
| a human customizing the CLI | mod files in `~/.config/haest/mods/` |
| a project shipping rules with its repo | mod files in `.haest/mods/`, which the human turns on with `project_mods` in a directory they trust |
