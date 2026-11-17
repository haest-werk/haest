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
| Model and prompt | [`model`](#model), [`effort`](#effort), [`system_prompt`](#system_prompt), [`append_system_prompt`](#append_system_prompt), [`provider_routing`](#provider_routing) |
| Context | [`context`](#context), [`skills`](#skills), [`skill_catalog`](#skill_catalog) |
| Tools | [`tools`](#tools), [`code`](#code), [`code_tools`](#code_tools), [`agents`](#agents), [`web_search`](#web_search), [`image`](#image), [`image_model`](#image_model), [`image_models`](#image_models), [`mcp`](#mcp), [`deferred_tools`](#deferred_tools) |
| Workspace, mods and trust | [`workspace`](#workspace), [`env_passthrough`](#env_passthrough), [`env`](#env), [`mods`](#mods), [`project_mods`](#project_mods), [`approvals`](#approvals), [`trusted_dirs`](#trusted_dirs) |
| Caps | [`max_turn_usd`](#max_turn_usd), [`max_turn_tokens`](#max_turn_tokens), [`max_request_tokens`](#max_request_tokens), [`max_turn_steps`](#max_turn_steps), [`max_turn_minutes`](#max_turn_minutes), [`max_repeats`](#max_repeats), [`max_conversation_usd`](#max_conversation_usd), [`max_conversation_tokens`](#max_conversation_tokens), [`wind_down_at`](#wind_down_at), [`context_window`](#context_window), [`compact_at`](#compact_at) |
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

- Takes: a list: comma-separated, or a TOML array; `none` is empty
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

### `approvals`

The built-in approval mod: asks before a tool changes anything.

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

- Takes: text
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
