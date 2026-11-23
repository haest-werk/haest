# How to

One recipe per task, each in the forms it comes in: a config entry for the `haest` command, a
mod file (TypeScript or JavaScript, run in embedded QuickJS), and Rust for a host that embeds
haest. [extending.md](extending.md) is the reference behind every one: what each event may
decide, in which order hooks run, what fails how.

New to haest? Start with [quickstart.md](quickstart.md).

| Task | Recipe |
|---|---|
| Refuse or rewrite a tool call | [Guard a tool](#guard-a-tool) |
| Send a turn back until a check passes | [Keep a turn going](#keep-a-turn-going) |
| Give the model a tool from a mod file | [Add a tool from a mod file](#add-a-tool-from-a-mod-file) |
| Edit a mod file while haest runs | [Reload mod files as they change](#reload-mod-files-as-they-change) |
| Add, replace or hide a host tool mid-conversation | [Change the host's tools while it runs](#change-the-hosts-tools-while-it-runs) |
| Keep long conversations small | [Compact on your own terms](#compact-on-your-own-terms) |
| Pay less for the prompt cache | [Tune the prompt cache](#tune-the-prompt-cache) |
| Run a hook as a service | [Hook a URL](#hook-a-url) |

## Guard a tool

A `tool.before` hook sees each call before the tool runs and may pass it, refuse it (the model
reads the reason) or replace its input. A `matcher` narrows it to one tool, or a prefix ending
in `*`.

A Claude Code `PreToolUse` script, as it is, from `~/.config/haest/config.toml`
([no-rm-rf.sh](examples/hooks/no-rm-rf.sh)):

```toml
[[hooks."tool.before"]]
command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
dialect = "claude"
matcher = "Bash"
```

A mod file:

```ts
export default function (haest: any): void {
  haest.on('tool.before', (call: any) => {
    if (call.name === 'Bash' && /rm -rf/.test(call.input.command ?? ''))
      return { deny: 'rm -rf is not allowed here' }
    return undefined // pass; { input: {..} } replaces the input
  })
}
```

Rust: `Hooks::new().on_tool_before(Binding::new(closure).with_matcher("Bash"))`, the closure
answering `Before::Pass`, `Before::Deny(reason)` or `Before::Input(map)`
([quickstart.md](quickstart.md#3-add-a-hook) has it whole). The script from Rust is
`ShellHook::new("sh no-rm-rf.sh").with_dialect(Claude)` in that binding; its `session_id` is
the session the mods' `ModEnv` names (`with_session`, and `set_session` at a /clear). A guard
that fails or times out refuses the call (`on_error = "block"`, the default on `tool.before`).

## Keep a turn going

A `turn.end` hook runs as the model stops on its own. `{ continue: text }` sends `text` back as
the next user message; the turn's `continued` says whether a `turn.end` decision already did,
so a hook that sends it back once checks it.

```ts
export default function (haest: any): void {
  haest.on('turn.end', (turn: any) =>
    turn.continued ? undefined : { continue: 'Run the tests before you stop.' })
}
```

As a command (`[[hooks."turn.end"]]` with `command = ...`) it prints that JSON, or exits 2
with the text on stderr; in the `claude` dialect it is Claude Code's `Stop` hook
(`{ "decision": "block", "reason": ... }`).
[external_hooks.rs](../crates/haest/examples/external_hooks.rs) runs a reviewer service that
does it, against a scripted model.

## Add a tool from a mod file

```ts
export default function (haest: any): void {
  haest.tool({
    name: 'word_count',
    description: 'Count the words in a text.',
    parameters: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    readOnly: true,
    execute: (input: any) => `words=${input.text.split(/\s+/).filter(Boolean).length}`,
  })
}
```

`execute` returns a string or `{ content, isError }`. A mod file also has
`haest.removeTool(name)` (a tool it added), `haest.disableTool(name)` and
`haest.enableTool(name)` (any tool, whoever added it), `haest.command(name, run)` (a `/name`
command) and `haest.notice(text)` (a line for the human). A change shows in the model's next
request.

## Reload mod files as they change

```toml
mods = true           # your mods in <config dir>/mods
mod_reload = "ask"    # or "auto"; "off" is the default
```

Before each model call haest reads the files again. A new or changed file loads, a deleted one
unloads with its tools; under `ask` each change waits for your Allow, and a Deny keeps the
version that runs. A file that fails to load keeps the version before it, and haest says why.
`mod_reload` comes only from your own config, a variable or a flag, never a project's file.
Rust mods never reload.

## Change the host's tools while it runs

Hand the agent a [`LiveTools`](../crates/haest-core/src/tool/live.rs) and keep a clone:

```rust
let live = LiveTools::new();
live.set_tool(Search::new());                 // add, or replace in its place
let deps = AgentDeps::new(shared, live.clone(), "You research.");
// later, from anywhere, between two model calls:
live.set_enabled("Search", false);            // hidden from the model, back in its place later
live.remove_tool("Search");                   // gone
live.set_tools(reloaded);                     // the whole set at once, e.g. a Vec<ToolRef>
```

A call already running finishes on the tool it started with. On a model whose catalog row has
`cache.toolChanges` (Anthropic's own API for Claude Opus 5.5 and Sonnet 5.5), a change costs no
cache miss; elsewhere the next call pays one ([extending.md](extending.md#tools-that-change-while-the-conversation-runs)).

## Compact on your own terms

Three ways, by what decides:

| Decides | How |
|---|---|
| a size | the built-in threshold: 80% of the window, or `compact_at` tokens (default 400000) when that is lower |
| your own rule, as a turn ends | a `turn.end` hook answering `{ compact: { instructions } }`: [auto-compact.ts](examples/mods/auto-compact.ts), or its Rust twin in [auto_compact.rs](../crates/haest/examples/auto_compact.rs) |
| the cache's clock | `[cache] compact_before_expire = { min_context = 0.6 }` (a share of the window; or a number of tokens, `120000`): an idle conversation compacts while its cache is still warm, when that costs less than writing it again |

`instructions` are added to haest's own summary prompt ("keep every file path"). Every
compaction reaches the host's `AgentHooks::on_compaction`, so a saved session records it.

## Tune the prompt cache

```toml
[cache]
system = "1h"         # the system prompt's breakpoint: off, 5m (default) or 1h
keep_warm = true      # ping a little before an idle conversation's cache expires
keep_warm_minutes = 60  # for at most this long after the last turn
compact_before_expire = { min_context = 120000, instructions = "Keep the open tasks." }

[cache.models."openrouter:google/*"]
history = "off"
```

`1h` costs more to write (2x the input price against 1.25x on Anthropic) and pays when you
pause longer than five minutes. `keep_warm` pings only while the model's prices say a ping
costs less than writing the cache again, and only on Anthropic's API and OpenRouter's
`anthropic/` models. A host with its own rules implements `CachePolicy`
([cache_policy.rs](../crates/haest/examples/cache_policy.rs)).

## Hook a URL

```toml
[[hooks."turn.end"]]
url = "https://review.internal/haest/turn-end"
headers = { authorization = "Bearer ${REVIEW_TOKEN}" }
timeout = 10
```

Each event is a POST of its JSON; the response body is the decision (empty passes). `${NAME}`
is filled in from haest's environment. A `url` entry is read only from your own config, a
variable or a flag, never from a project's file. In Rust it is `HttpHook::new(url)`, bound like
any hook.
