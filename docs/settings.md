# haest settings

<!-- Written from the settings registry (crates/haest-config/src/registry.rs).
     Edit the registry, then rewrite this file:
     HAEST_BLESS=1 cargo test -p haest-config reference -->

haest starts bare: no tools, no instruction files, no skills, no MCP, no mods,
in the sandbox, with every brake on. Each setting below turns one thing on or
sets one value: a model, a cap, a path.

A setting has one name, written three ways: a key in a TOML config file, a
`HAEST_*` variable, a flag. Each layer wins over the ones before it:

1. the defaults below
2. your `~/.config/haest/config.toml` (`$XDG_CONFIG_HOME/haest`, or `config_dir`)
3. the nearest `.haest/config.toml` from the working dir up, inside `trusted_dirs` only
4. the profile `profile` names
5. `HAEST_*` variables
6. flags

`haest settings` shows what a run gets and where each value came from; it takes
the same flags. [environment.md](environment.md) covers what haest reads from
its environment and what a process it starts gets. [examples/](examples/) has
whole config files.

| Group | Settings |
|---|---|
| Profile | [`profile`](#profile) |
| Model and prompt | [`model`](#model), [`effort`](#effort), [`cache`](#cache), [`system_prompt`](#system_prompt), [`append_system_prompt`](#append_system_prompt), [`provider_routing`](#provider_routing) |
| Context | [`context`](#context), [`skills`](#skills), [`skill_catalog`](#skill_catalog) |
| Tools | [`tools`](#tools), [`code`](#code), [`code_tools`](#code_tools), [`agents`](#agents), [`web_search`](#web_search), [`image`](#image), [`image_model`](#image_model), [`image_models`](#image_models), [`mcp`](#mcp), [`deferred_tools`](#deferred_tools) |
| Workspace, mods and trust | [`workspace`](#workspace), [`env_passthrough`](#env_passthrough), [`env`](#env), [`mods`](#mods), [`project_mods`](#project_mods), [`mod_reload`](#mod_reload), [`hooks`](#hooks), [`approvals`](#approvals), [`trusted_dirs`](#trusted_dirs) |
| Caps | [`max_turn_usd`](#max_turn_usd), [`max_turn_tokens`](#max_turn_tokens), [`max_request_tokens`](#max_request_tokens), [`max_turn_steps`](#max_turn_steps), [`max_turn_minutes`](#max_turn_minutes), [`max_repeats`](#max_repeats), [`max_conversation_usd`](#max_conversation_usd), [`max_conversation_tokens`](#max_conversation_tokens), [`wind_down_at`](#wind_down_at), [`context_window`](#context_window), [`compact_at`](#compact_at), [`timeouts`](#timeouts), [`limits`](#limits) |
| Subagents | [`max_agent_turns`](#max_agent_turns), [`max_agent_depth`](#max_agent_depth), [`max_subagents`](#max_subagents), [`subagent_usd`](#subagent_usd), [`subagent_steps`](#subagent_steps), [`subagent_minutes`](#subagent_minutes), [`subagent_budgets`](#subagent_budgets) |
| Watching and driving | [`event_ring`](#event_ring), [`stdio`](#stdio), [`control_socket`](#control_socket), [`control_dir`](#control_dir), [`broker_url`](#broker_url), [`broker_access`](#broker_access) |
| Paths | [`data_dir`](#data_dir), [`config_dir`](#config_dir), [`debug`](#debug) |
| [Profiles](#profiles) | the built-in ones, and your own |

## Profile

### `profile`

A named set of settings: claude (built in: everything Claude Code has that haest reads), or a [profiles.&lt;name>] table in a config file.

- Takes: text
- Default: unset
- Set in: a config file (yours, or a trusted project's), its variable, its flag; not inside a profile

```text
config.toml   profile = "claude"
variable      HAEST_PROFILE=claude
flag          --profile claude
```

## Model and prompt

### `model`

provider:model.

- Takes: text
- Default: `openrouter:anthropic/claude-sonnet-5.5`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   model = "anthropic:claude-opus-5-5"
variable      HAEST_MODEL=anthropic:claude-opus-5-5
flag          --model anthropic:claude-opus-5-5   or   -m anthropic:claude-opus-5-5
```

### `effort`

Reasoning effort (unset: the model's own default).

- Takes: one of `off`, `low`, `medium`, `high`, `xhigh`, `max`
- Default: unset
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   effort = "high"
variable      HAEST_EFFORT=high
flag          --effort high   or   -e high
```

### `cache`

The prompt cache: which blocks carry a breakpoint and for how long (system, tools, history: off, 5m or 1h), and what an idle conversation does before its cache expires (keep_warm, keep_warm_minutes, compact_before_expire); per model under models."&lt;spec or prefix*>".
`system`, `tools` and `history` (the newest message, which the next request reads the conversation before it from) each take `off`, `5m` or `1h`; the default is `5m` for all three. `1h` costs more to write (2x the input price against 1.25x on Anthropic) and pays when you often pause for longer than five minutes; a model that keeps no cache entry for an hour gets five minutes, and you are told. `keep_warm = true` sends a pre-warm a little before an idle conversation's cache expires, while the model's prices say the pings cost less than writing the cache again, for at most `keep_warm_minutes` after the last turn (default 60); a ping that wrote instead of read stops it, and you are told. A model with no price, or a provider that takes no pre-warm (only Anthropic's API and OpenRouter's `anthropic/` models do), is never pinged. `compact_before_expire = { min_context = 120000, instructions = "..." }` (`min_context` is a whole number of tokens, or a share of the window between 0 and 1: `0.6`; default `"off"`) compacts an idle conversation that holds at least `min_context` while its cache is still warm, when the summary and the smaller next write cost less than writing the whole prompt again; when they would not, `keep_warm` still pings. Only a conversation that stays open between turns idles: the interactive prompt, or a session another program drives. `[cache.models."openrouter:google/*"]` sets any of these for the models it matches (exact, or `prefix*`); the most specific wins. A key that is neither (a regular expression, a `*` inside) would match no model, and is refused.

- Takes: a table: `[cache]` in a file; in a variable or a flag, its keys as TOML (`key = value` lines, or `{ key = value }`), the flag repeated for more. Each layer merges into the ones before it, key by key
- Default: every key at the default given above
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   [cache]
              keep_warm = true
              system = "1h"
variable      HAEST_CACHE='{ system = "1h", keep_warm = true }'
flag          --cache '{ system = "1h", keep_warm = true }'
```

### `system_prompt`

Replaces haest's own identity text at the top of the system prompt.

- Takes: text
- Default: unset
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   system_prompt = "You are a terse code reviewer."
variable      HAEST_SYSTEM_PROMPT='You are a terse code reviewer.'
flag          --system-prompt 'You are a terse code reviewer.'
```

### `append_system_prompt`

Text appended to the system prompt.

- Takes: text
- Default: unset
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   append_system_prompt = "Answer in British English."
variable      HAEST_APPEND_SYSTEM_PROMPT='Answer in British English.'
flag          --append-system-prompt 'Answer in British English.'
```

### `provider_routing`

OpenRouter's provider routing, as JSON: {"global": {"banned": [...], "favorites": [...]}, "overrides": ..., "project": ...}.

- Takes: text
- Default: unset
- Set in: your own config file, its variable, its flag; never a project's file or a profile

```text
config.toml   provider_routing = '{"global": {"banned": ["novita"]}}'
variable      HAEST_PROVIDER_ROUTING='{"global": {"banned": ["novita"]}}'
flag          --provider-routing '{"global": {"banned": ["novita"]}}'
```

## Context

### `context`

Instruction files: agents (AGENTS.md: ~/.agents/AGENTS.md, then every dir down to here), claude (CLAUDE.md, .claude/CLAUDE.md, CLAUDE.local.md, ~/.claude/CLAUDE.md).

- Takes: a list of `agents`, `claude`; `all` (every one) or `none`
- Default: none
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   context = ["agents"]
variable      HAEST_CONTEXT=agents
flag          --context agents
```

### `skills`

Where skills come from: agents (~/.agents/skills, .agents/skills), claude (~/.claude/skills, .claude/skills), plugins (Claude Code's installed plugins); /&lt;name> runs one.

- Takes: a list of `agents`, `claude`, `plugins`; `all` (every one) or `none`
- Default: none
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   skills = ["agents", "claude"]
variable      HAEST_SKILLS=agents,claude
flag          --skills agents,claude
```

### `skill_catalog`

List the skills in the system prompt, so the model picks them on its own.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   skill_catalog = true
variable      HAEST_SKILL_CATALOG=true
flag          --skill-catalog
```

## Tools

### `tools`

Built-in tools: read, write, edit, bash, grep, glob, web_fetch, tasks, skill (all = every one).

- Takes: a list of `read`, `write`, `edit`, `bash`, `grep`, `glob`, `web_fetch`, `tasks`, `skill`; `all` (every one) or `none`
- Default: none
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   tools = ["read", "grep", "glob", "bash"]
variable      HAEST_TOOLS=read,grep,glob,bash
flag          --tools read,grep,glob,bash
```

### `code`

The Code tool: a JavaScript run that calls the other tools.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   code = true
variable      HAEST_CODE=true
flag          --code
```

### `code_tools`

Extra tool names Code may call (exact, or prefix*).

- Takes: a list of names, each exact (`Bash`) or a prefix ending in `*` (`mcp__github__*`): comma-separated, or a TOML array; `none` is empty. A regular expression is refused (`Edit|Write` is two names), and so is a `*` anywhere but the end
- Default: none
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   code_tools = ["Bash", "mcp__github__*"]
variable      HAEST_CODE_TOOLS='Bash,mcp__github__*'
flag          --code-tools 'Bash,mcp__github__*'
```

### `agents`

Agent, SendMessage and StopAgent: subagents.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   agents = true
variable      HAEST_AGENTS=true
flag          --agents
```

### `web_search`

The provider's web search, by engine.

- Takes: one of `off`, `auto`, `native`, `exa`, `parallel`, `firecrawl`, `perplexity`
- Default: `off`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   web_search = "native"
variable      HAEST_WEB_SEARCH=native
flag          --web-search native
```

### `image`

GenerateImage, over OpenRouter (needs OPENROUTER_API_KEY).

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   image = true
variable      HAEST_IMAGE=true
flag          --image
```

### `image_model`

GenerateImage's default model.

- Takes: text
- Default: unset: the tool's own, `google/gemini-nano-banana-2.1`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   image_model = "black-forest-labs/flux-3-image"
variable      HAEST_IMAGE_MODEL=black-forest-labs/flux-3-image
flag          --image-model black-forest-labs/flux-3-image
```

### `image_models`

The other image models a call may pick.

- Takes: a list: comma-separated, or a TOML array; `none` is empty
- Default: unset: the tool's own, `google/gemini-nano-banana-2.1`, `openai/gpt-image-2.6-sunburst`, `black-forest-labs/flux-3-image`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   image_models = ["openai/gpt-image-2.6-sunburst", "black-forest-labs/flux-3-image"]
variable      HAEST_IMAGE_MODELS=openai/gpt-image-2.6-sunburst,black-forest-labs/flux-3-image
flag          --image-models openai/gpt-image-2.6-sunburst,black-forest-labs/flux-3-image
```

### `mcp`

Start the project's MCP servers (.mcp.json); a trusted dir only.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   mcp = true
variable      HAEST_MCP=true
flag          --mcp
```

### `deferred_tools`

How MCP tools reach the model: behind ToolSearch (proxy, grow) or all up front (off).
ToolSearch lists each held-back tool with its description's first sentence. A host linking haest can hold back its own tools too, all but a list it names (`ToolDeferral::deferring_all_except`, `DeferredTools`); a held-back tool the model calls by name still runs, and loads.

- Takes: one of `off`, `proxy`, `grow`
- Default: `proxy`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   deferred_tools = "grow"
variable      HAEST_DEFERRED_TOOLS=grow
flag          --deferred-tools grow
```

## Workspace, mods and trust

### `workspace`

sandbox: a simulated bash over a copy-on-write overlay (nothing reaches the disk until /apply); real: the real disk and bash.

- Takes: one of `sandbox`, `real`
- Default: `sandbox`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   workspace = "real"
variable      HAEST_WORKSPACE=real
flag          --workspace real   or   -w real
```

### `env_passthrough`

What a process haest starts (bash, a background job, an MCP server) gets of haest's environment: none, core (PATH, HOME, the user, the locale, the temp dirs), all (every variable, API keys included).

- Takes: one of `none`, `core`, `all`
- Default: `core`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   env_passthrough = "none"
variable      HAEST_ENV_PASSTHROUGH=none
flag          --env-passthrough none
```

### `env`

A variable every process haest starts gets, the sandbox's shell included, over env_passthrough: NAME=VALUE, or NAME for haest's own value; repeat it for more. In a file: an [env] table. ${NAME} and ${NAME:-default} in a value are filled in from haest's environment.

- Takes: `NAME=VALUE`, or `NAME` for haest's own value; repeat the flag for more
- Default: none
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   [env]
              GH_TOKEN = "${GITHUB_TOKEN}"
variable      HAEST_ENV='GH_TOKEN=${GITHUB_TOKEN}'
flag          --env 'GH_TOKEN=${GITHUB_TOKEN}'   or   -E 'GH_TOKEN=${GITHUB_TOKEN}'
```

### `mods`

Your mods in &lt;config dir>/mods.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   mods = true
variable      HAEST_MODS=true
flag          --mods
```

### `project_mods`

The project's mods in .haest/mods (its code runs; a trusted dir only).

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   project_mods = true
variable      HAEST_PROJECT_MODS=true
flag          --project-mods
```

### `mod_reload`

Whether mod files reload as they change, checked before each model call: off, ask (a new, changed or deleted file waits for your Allow) or auto (it loads or unloads without asking).
A deleted file unloads, and its tools leave the model's list. A file that fails to load keeps the version that ran, and haest says why. After a Deny the old version stays, and the same content is not asked about again. Rust mods never reload.

- Takes: one of `off`, `ask`, `auto`
- Default: `off`
- Set in: your own config file, its variable, its flag; never a project's file or a profile

```text
config.toml   mod_reload = "ask"
variable      HAEST_MOD_RELOAD=ask
flag          --mod-reload ask
```

### `hooks`

Commands and URLs run at each event of the mods protocol, with no Rust: [[hooks."&lt;event>"]] entries, each a command (run on the host) or a url (POSTed), in haest's JSON or Claude Code's (dialect = "claude").
The events are the protocol's own names, quoted for their dot: `tool.before`, `tool.after`, `prompt.submit`, `system.prompt`, `model.before`, `model.after`, `turn.end`. An entry takes `command` or `url` (one of them), `dialect` (`haest` or `claude`; `claude` runs a Claude Code hook script as it is, on the events Claude Code has, so never on `model.before` or `model.after`), `matcher` (on `tool.before` and `tool.after` only: a tool's name, or a prefix ending in `*`), `timeout` (seconds, Claude Code's key as it is: every other time setting that takes a number takes minutes, `[timeouts]` takes text with its unit, and the cache's TTLs are the words `5m` and `1h`; default `[timeouts] hook`, a minute), `on_error` (`block` or `continue`; default `block` on `tool.before`, else `continue`) and `headers` (a `url`'s, `{ name = "value" }`, `${NAME}` filled in from haest's environment). A command runs on the host (never in the sandbox) in the working dir, with the environment every process haest starts gets (`env_passthrough`, `env`): give one in your own file as an absolute path or under `~/`, since a relative one runs whatever the working dir has there. Every layer's entries bind after the ones before it, in the order written, as one mod after the built-in ones and your mod files. A subagent's tool calls pass through them and its system prompt takes what `system.prompt` adds; `prompt.submit`, `model.before`, `model.after` and `turn.end` are the main conversation's. `haest settings` shows a header by its name only. [hooks.md](hooks.md) has the whole story: every event's input and every answer it takes, in both dialects, and every key.

- Takes: a table: `[hooks]` in a file; in a variable or a flag, its keys as TOML (`key = value` lines, or `{ key = value }`), the flag repeated for more. Each layer merges into the ones before it, key by key
- Default: none
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag; a `url` entry only in your own file, its variable or its flag, never in a project's file or a profile one defines

```text
config.toml   [[hooks."tool.before"]]
              command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
              dialect = "claude"
              matcher = "Bash"
variable      HAEST_HOOKS='{ "tool.before" = [{ command = "sh ~/.config/haest/hooks/no-rm-rf.sh", dialect = "claude", matcher = "Bash" }] }'
flag          --hooks '{ "tool.before" = [{ command = "sh ~/.config/haest/hooks/no-rm-rf.sh", dialect = "claude", matcher = "Bash" }] }'
```

### `approvals`

The built-in approval mod: in the real workspace, asks before a shell command that cannot be undone (deleting recursively, rewriting git history or the remote, wiping a disk, killing processes by name, sudo); anything else runs without asking.
haest asks on a terminal only where the question can be seen and answered: stdin must be a terminal, and so must stderr, where the question shows (with --json it is a line on stdout, which must not be a regular file). Otherwise each approval is refused at once, and haest says why on stderr.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `true`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   approvals = false
variable      HAEST_APPROVALS=false
flag          --approvals=false
```

### `trusted_dirs`

Dirs whose project files haest reads (.haest/config.toml, .mcp.json, .haest/mods), with every dir inside them.

- Takes: a list: comma-separated, or a TOML array; `none` is empty
- Default: none
- Set in: your own config file, its variable, its flag; never a project's file or a profile

```text
config.toml   trusted_dirs = ["~/projects", "~/work"]
variable      HAEST_TRUSTED_DIRS='~/projects,~/work'
flag          --trusted-dirs '~/projects,~/work'
```

## Caps

### `max_turn_usd`

One turn's spend (0 = no cap).

- Takes: a number, 0 or more
- Default: `2`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_turn_usd = 5
variable      HAEST_MAX_TURN_USD=5
flag          --max-turn-usd 5
```

### `max_turn_tokens`

One turn's tokens (unset: 10x the model's window; 0 = no cap).

- Takes: a whole number, 0 or more
- Default: unset
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_turn_tokens = 2000000
variable      HAEST_MAX_TURN_TOKENS=2000000
flag          --max-turn-tokens 2000000
```

### `max_request_tokens`

One model call's input tokens (unset: 90% of the window; 0 = no cap).

- Takes: a whole number, 0 or more
- Default: unset
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_request_tokens = 150000
variable      HAEST_MAX_REQUEST_TOKENS=150000
flag          --max-request-tokens 150000
```

### `max_turn_steps`

Model calls in one turn (0 = no cap).

- Takes: a whole number, 0 or more
- Default: `100`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_turn_steps = 200
variable      HAEST_MAX_TURN_STEPS=200
flag          --max-turn-steps 200
```

### `max_turn_minutes`

One turn's wall clock (0 = no cap).

- Takes: a number, 0 or more
- Default: `60`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_turn_minutes = 30
variable      HAEST_MAX_TURN_MINUTES=30
flag          --max-turn-minutes 30
```

### `max_repeats`

The same tool call in a row before the guard stops it (0 = no cap).

- Takes: a whole number, 0 or more
- Default: `6`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_repeats = 10
variable      HAEST_MAX_REPEATS=10
flag          --max-repeats 10
```

### `max_conversation_usd`

The whole conversation's spend (0 = no cap).

- Takes: a number, 0 or more
- Default: `20`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_conversation_usd = 50
variable      HAEST_MAX_CONVERSATION_USD=50
flag          --max-conversation-usd 50
```

### `max_conversation_tokens`

Tokens of models with no price (0 = no cap).

- Takes: a whole number, 0 or more
- Default: `20000000`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_conversation_tokens = 50000000
variable      HAEST_MAX_CONVERSATION_TOKENS=50000000
flag          --max-conversation-tokens 50000000
```

### `wind_down_at`

At this share of a cap, no new subagents and the model is told once to conclude (0 = off).
Watches the conversation's USD and unpriced tokens, and the turn's USD, tokens and wall clock. A subagent watches the conversation's and its own (subagent_usd, subagent_minutes, max_turn_tokens) the same way, and winds down alone. One share for every cap. The guard's nudge at half a cap is separate and stays on.

- Takes: a number from 0 to 1
- Default: `0`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   wind_down_at = 0.8
variable      HAEST_WIND_DOWN_AT=0.8
flag          --wind-down-at 0.8
```

### `context_window`

The model's window in tokens (unset: the catalog's, else 200000).

- Takes: a whole number, 0 or more
- Default: unset
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   context_window = 1000000
variable      HAEST_CONTEXT_WINDOW=1000000
flag          --context-window 1000000
```

### `compact_at`

Compact the conversation past this many tokens (0 = at 80% of the window only).

- Takes: a whole number, 0 or more
- Default: `400000`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   compact_at = 300000
variable      HAEST_COMPACT_AT=300000
flag          --compact-at 300000
```

### `timeouts`

How long haest waits before it gives up: on a hook (hook), on a human's Allow/Deny (ask), on a shell command (bash, bash_max), on a model's answer to start (model_headers), on an MCP server (mcp_connect, mcp_call), on a web page (web_fetch), and on its JavaScript (javascript: code, sandbox, mod_load, mod_call, mod_slice); each is text with its unit, like "30s" or "10m".
A value is a number and its unit, `ms`, `s`, `m` or `h` (`"500ms"`, `"30s"`, `"1.5h"`), above 0. A number alone is refused: `30` could be seconds or minutes. `hook` (default `"60s"`): a `[[hooks.&lt;event>]]` entry that sets no `timeout` of its own; past it, the hook failed and its `on_error` decides. `ask` (`"10m"`): an Allow/Deny prompt, the approval mod's or a mod's; unanswered, it is a Deny. `bash` (`"2m"`): a Bash call that names no timeout of its own. `bash_max` (`"10m"`): the most a Bash call may ask for, and the most one sandbox shell call runs; a `bash` above it raises it to `bash`. A background job has no timeout. `model_headers` (`"10m"`): one try of a model call, waiting for the provider's status and headers; past it the try is retried like a dropped connection. A stream that has started is not timed by it. A host that hands in its own models keeps their own. `mcp_connect` (`"15s"`): an MCP server's start and `initialize`, and again its tool list; past it the server did not start. A server run with `npx` or `uvx` may need longer the first time, while its package downloads. `mcp_call` (`"10m"`): one call to an MCP server's tool; past it the call fails and is cancelled on the server. `web_fetch` (`"30s"`): one WebFetch call, every redirect and the body included. `javascript.code` (`"10m"`): a Code tool run, wall clock, the tools it calls included; the model may ask for less on a call, never for more. `javascript.sandbox` (`"5m"`): the sandbox `js` command, never past the shell call it runs in. `javascript.mod_load` (`"30s"`): a mod file's default export, as the mod loads. `javascript.mod_call` (`"60s"`): one call into a loaded mod file (a handler, a tool, a command), awaiting included, the time a `haest.ask` waits on the human not; past it that call fails and the mod runs on. `javascript.mod_slice` (`"10s"`): a mod file's code without a pause, what stops a CPU loop in a mod. The turn's caps (`max_turn_minutes`, `subagent_minutes`) hold over all of it.

- Takes: a table: `[timeouts]` in a file; in a variable or a flag, its keys as TOML (`key = value` lines, or `{ key = value }`), the flag repeated for more. Each layer merges into the ones before it, key by key
- Default: every key at the default given above
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   [timeouts]
              hook = "30s"
              
              [timeouts.javascript]
              code = "20m"
variable      HAEST_TIMEOUTS='{ hook = "30s", javascript = { code = "20m" } }'
flag          --timeouts '{ hook = "30s", javascript = { code = "20m" } }'
```

### `limits`

How much memory each kind of JavaScript run gets (javascript: code_memory, sandbox_memory, mod_memory); each is text with its unit, like "64MiB" or "1GiB".
A value is a number and its unit, `B`, `KB`, `MB`, `GB` (powers of 1000) or `KiB`, `MiB`, `GiB` (powers of 1024), in any case, 1MiB at least. A number alone is refused. Each is one engine's heap: an allocation past it is an out-of-memory error inside the script. `javascript.code_memory` (default `"64MiB"`): a Code tool run. `javascript.sandbox_memory` (`"64MiB"`): the sandbox `js` command. `javascript.mod_memory` (`"128MiB"`): each mod file, for as long as it is loaded.

- Takes: a table: `[limits]` in a file; in a variable or a flag, its keys as TOML (`key = value` lines, or `{ key = value }`), the flag repeated for more. Each layer merges into the ones before it, key by key
- Default: every key at the default given above
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   [limits.javascript]
              code_memory = "256MiB"
variable      HAEST_LIMITS='{ javascript = { code_memory = "256MiB" } }'
flag          --limits '{ javascript = { code_memory = "256MiB" } }'
```

## Subagents

### `max_agent_turns`

Turns in a row a job or an agent starts with no human message; past it haest holds them (0 = no cap).

- Takes: a whole number, 0 or more
- Default: `30`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_agent_turns = 10
variable      HAEST_MAX_AGENT_TURNS=10
flag          --max-agent-turns 10
```

### `max_agent_depth`

How deep agents nest, main = 0 (0 = no Agent tool).

- Takes: a whole number, 0 or more
- Default: `2`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_agent_depth = 1
variable      HAEST_MAX_AGENT_DEPTH=1
flag          --max-agent-depth 1
```

### `max_subagents`

Subagents running at once, across the tree (0 = no Agent tool).

- Takes: a whole number, 0 or more
- Default: `4`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   max_subagents = 8
variable      HAEST_MAX_SUBAGENTS=8
flag          --max-subagents 8
```

### `subagent_usd`

One subagent's spend.

- Takes: a number, 0 or more
- Default: `1`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   subagent_usd = 0.5
variable      HAEST_SUBAGENT_USD=0.5
flag          --subagent-usd 0.5
```

### `subagent_steps`

One subagent's model calls.

- Takes: a whole number, 0 or more
- Default: `50`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   subagent_steps = 25
variable      HAEST_SUBAGENT_STEPS=25
flag          --subagent-steps 25
```

### `subagent_minutes`

One subagent's wall clock.

- Takes: a number, 0 or more
- Default: `15`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   subagent_minutes = 5
variable      HAEST_SUBAGENT_MINUTES=5
flag          --subagent-minutes 5
```

### `subagent_budgets`

The Agent tool takes a budget {usd, minutes}: tighter caps for one subagent, its spend held on the conversation's budget while it runs, its time fitted to what is left of the turn.
Off, the Agent tool's parameters are Claude Code's. On, each value only tightens `subagent_usd` and `subagent_minutes` (an absent one is the cap); a spawn whose spend or time does not fit what is left is refused with what is left, so the model can ask for less. Subagents running at once hold their spend together, so they cannot overshoot `max_conversation_usd` together.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   subagent_budgets = true
variable      HAEST_SUBAGENT_BUDGETS=true
flag          --subagent-budgets
```

## Watching and driving

### `event_ring`

Frames kept for a peer that catches up on the conversation's stream, besides the running turn.

- Takes: a whole number, 0 or more
- Default: `1000`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   event_ring = 5000
variable      HAEST_EVENT_RING=5000
flag          --event-ring 5000
```

### `stdio`

Be driven by the parent process: requests in on stdin, frames out on stdout, no terminal.
One JSON object per line both ways, in haest-wire's format: `hello` first, then every frame of the conversation from the start, and a `reply` to each request (a turn's at its end). The parent may do everything a terminal may: turns (a person's input), `interrupt`, model and effort changes, `clear`, `close`, and the answers to approval asks. haest runs until stdin closes, once what it was asked has finished, or until a `close`. Never from a file: a config file cannot turn a terminal into a pipe.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: its variable or its flag only, never a config file

```text
config.toml   (not in a file)
variable      HAEST_STDIO=true
flag          --stdio
```

### `control_socket`

A Unix socket another program on this machine reaches the conversation through (haest ctl): off, watch (its stream, a snapshot, the history) or command (turns, interrupts, changes and approval answers too).
The socket goes in `control_dir`, named after the conversation's stream id and readable by you only (the dir 0700, the socket 0600), with a discovery file beside it that `haest ctl list` reads; both go when haest exits. What arrives over a command socket is another program's input: it counts against `max_agent_turns`, and at an open prompt it waits for Enter. Never from a project's file: a repository cannot open a socket into your conversation.

- Takes: one of `off`, `watch`, `command`
- Default: `off`
- Set in: your own config file, its variable, its flag; never a project's file or a profile

```text
config.toml   control_socket = "watch"
variable      HAEST_CONTROL_SOCKET=watch
flag          --control-socket watch
```

### `control_dir`

Where control sockets and their discovery files go (unset: &lt;data_dir>/control).

- Takes: a directory: `~/x` is under your home dir, `x` under the working dir
- Default: unset
- Set in: your own config file, its variable, its flag; never a project's file or a profile

```text
config.toml   control_dir = "~/.haest/control"
variable      HAEST_CONTROL_DIR='~/.haest/control'
flag          --control-dir '~/.haest/control'
```

### `broker_url`

A broker the conversation links to over a WebSocket (ws:// or wss://), so a fleet view elsewhere watches it; its token is the key HAEST_BROKER_TOKEN.
haest dials out: the first frame is `hello` with the token in it (never in the URL), the broker answers with the last seq it holds, and the stream resumes there; a link that drops is dialed again, after a random wait that doubles each try (at most 30 s), and so is one the broker goes silent on (haest pings a broker it has not heard from in 20 s, and gives up on it after 60 s of silence). A broker that refuses the token (close 4401 or 4408, or a 401 or 403 handshake) is not dialed again, and haest says so. Never from a project's file: a repository cannot send your conversation somewhere.

- Takes: a URL; one with an `@` past its host is refused: a URL parser ends the host at the first `/`, `?` or `#`, so a password holding one would send the rest of it, and the host after the `@`, to another host. Percent-encode the password (`/` as `%2F`, `?` as `%3F`, `#` as `%23`, `\` as `%5C`, `@` as `%40`). A URL with an `@` meant for its path or query cannot be used as it is: `%40` there makes another URL, which the server may or may not read the same. One with no host is refused too (`wss:///agents` or `wss://:8443/agents`, what an empty `${HOST}` makes): a parser takes the first part of its path for the host, or dials nothing
- Default: unset
- Set in: your own config file, its variable, its flag; never a project's file or a profile

```text
config.toml   broker_url = "wss://broker.example/agents"
variable      HAEST_BROKER_URL=wss://broker.example/agents
flag          --broker-url wss://broker.example/agents
```

### `broker_access`

What the broker may do: watch (the stream, a snapshot, the history) or command (turns, interrupts, changes and approval answers too).

- Takes: one of `watch`, `command`
- Default: `watch`
- Set in: your own config file, its variable, its flag; never a project's file or a profile

```text
config.toml   broker_access = "command"
variable      HAEST_BROKER_ACCESS=command
flag          --broker-access command
```

## Paths

### `data_dir`

Sessions, history and spill files (unset: $XDG_DATA_HOME/haest, else ~/.local/share/haest).

- Takes: a directory: `~/x` is under your home dir, `x` under the working dir
- Default: `~/.local/share/haest`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   data_dir = "~/.haest/data"
variable      HAEST_DATA_DIR='~/.haest/data'
flag          --data-dir '~/.haest/data'
```

### `config_dir`

config.toml, providers and mods (unset: $XDG_CONFIG_HOME/haest, else ~/.config/haest).

- Takes: a directory: `~/x` is under your home dir, `x` under the working dir
- Default: `~/.config/haest`
- Set in: its variable or its flag only, never a config file

```text
config.toml   (not in a file)
variable      HAEST_CONFIG_DIR='~/dotfiles/haest'
flag          --config-dir '~/dotfiles/haest'
```

### `debug`

Log what haest does under the hood.

- Takes: `true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`
- Default: `false`
- Set in: a config file (yours, or a trusted project's), a profile, its variable, its flag

```text
config.toml   debug = true
variable      HAEST_DEBUG=true
flag          --debug
```

## Profiles

`profile` picks a named set of settings. It sits over the config files and
under the variables and flags. A `[profiles.<name>]` table in a config file
adds to a built-in profile (its keys win) or makes a new one:

```toml
profile = "review"

[profiles.review]
tools = ["read", "grep", "glob"]
```

### `claude` (built in)

| Setting | Value |
|---|---|
| [`context`](#context) | `agents,claude` |
| [`skills`](#skills) | `all` |
| [`skill_catalog`](#skill_catalog) | `true` |
| [`tools`](#tools) | `all` |
| [`agents`](#agents) | `true` |
| [`web_search`](#web_search) | `native` |
| [`mcp`](#mcp) | `true` |
| [`workspace`](#workspace) | `real` |
| [`env_passthrough`](#env_passthrough) | `all` |
