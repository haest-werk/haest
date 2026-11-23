# haest in Rust: haest + the haest CLI

**Goal.** One Rust agent loop and tool harness that every haest host embeds:
the `haest` CLI, brain, and later remote-claude's agent host. The loop began
as TypeScript (`remote-claude/src/haest-agent-host`, about 7.3k lines of core,
providers and tools, plus 3.7k of tests), and brain ran a vendored copy of it
that drifted. brain now links this library (D6), so the copy is gone.

**The port's source of truth** was the TypeScript haest in remote-claude's
`src/haest-agent-host/` (`core/`, `providers/`, `tools/`). Behaviour came from
there, file by file, tests included. Since 0.2.0 the Rust haest leads.

## Decisions

| Code | Decision | Why |
|---|---|---|
| D1 | Standalone repo `haest`, a Cargo workspace | Sharable by remote-claude AND brain; remote-claude's repo is too heavy to be a git dependency |
| D2 | Crates: `haest-core`, `haest-providers`, `haest-workspace`, `haest-tools`, `haest-cli`, and `haest` (the facade: `use haest::...`, features pick the parts) | Codex's split (`codex-rs`) is the precedent; a host links only what it uses |
| D3 | The session JSONL is the TS haest's format, byte compatible (camelCase keys, same record kinds) | Either haest resumes the other's session; brain's conversation files keep working |
| D4 | haest-core has its OWN `Fs` trait; `haest-workspace` adapts any `Fs` into bashkit's `FileSystem` | haest-core stays free of a pre-1.0 dependency, and a host's own files (BrainFs) still get a sandboxed bash |
| D5 | P1 default: haest mods run in embedded QuickJS (`rquickjs`), no `node:` APIs | The Code tool needs QuickJS anyway (wave 2) |
| D6 | P2: brain links `haest` IN-PROCESS (`default-features = false`, features `providers`, `workspace`, `tools`, `settings`) and drops the vendored TS haest from its Bun rooms child. Ruled 2026-11-11; it replaces the first default, haest as brain's own child process holding no keys | One process, no wire between brain and its loop. The cost: the loop runs in the process that holds the keys, so the boundary is D15/D16 alone (a run touches only the `World`, `KeySource` and settings brain hands it) |
| D7 | Every brain patch to the vendored copy is a plug point in the library from day 1 (table below) | Otherwise brain forks again |
| D8 | ONE settings loader (`haest-config`): bare < `<config dir>/config.toml` < a trusted project's `.haest/config.toml` < the profile < `HAEST_*` < flags. One name per setting: `snake_case` in TOML, `HAEST_UPPER_SNAKE`, `--kebab-case`. TOML only | The world's convention (Codex, cargo, git); one registry means a new setting is one row, never three parsers |
| D9 | Bare by default: every capability (tools, context files, skills, agents, Code, web search, image, MCP, mods) off until asked for; every brake (approvals, caps, sandbox) on | haest is "the opposite of `--bare`": a run carries only what it was given. Bare is 179 input tokens a turn, the claude profile 45,764 |
| D10 | Profiles: `claude` built in, `[profiles.<name>]` in any config file adds to it or defines a new one; `--profile` / `HAEST_PROFILE` | One switch for "behave like Claude Code", and named sets for a host's kinds of run (brain's `dream`) |
| D11 | The `claude` profile is what haest READS of Claude Code, not parity; the gaps are listed (`profiles.rs`, README) | A profile that claimed parity would lie |
| D12 | A project's `.haest/config.toml`, `.mcp.json` and `.haest/mods` are read only inside `trusted_dirs`, which only the human's own file, env or flags set | A cloned repo must not configure, start commands or run code by itself (Codex's `trust_level`) |
| D13 | AGENTS.md is off in bare too (`context = ["agents"]` turns it on) | D9 has no exceptions |
| D14 | `haest-runtime` composes the conversation from what it is handed; the CLI is the runtime plus a terminal (`Ui`, `Prompter`); a host may take the pieces instead | Every host builds on the same library; the CLI owns display only |
| D15 | The library reads NOTHING it was not handed: no file, variable or flag of its own, no real disk unless handed `World::Disk`, no inherited env for a child. `haest-settings` is plain typed values (`Settings::bare`, word sets that fail to compile on a typo); `haest-config` is the OPT-IN loader (files, `HAEST_*`, flags) whose `Loaded` is passed straight into `RuntimeInputs` | A host with hardcoded settings (brain) needs no loader, and the node's environment cannot change what a run may do. Config files are the loader's; content files (AGENTS.md, skills, sessions, mods) stay the runtime's, through the handed `Fs` |
| D16 | `RuntimeInputs { settings, world, passed_env, keys, providers, mcp_servers, routing, http }`. `World::Disk { child_env }` or `World::Fs(fs)` (the host's files: the sandbox over them, no real process, no stdio MCP server). Provider keys through a `KeySource` (a closure, a map, a vault). Trust is the loader's: it lists a project's MCP servers and turns its mods on only in a trusted dir | Each thing a run may touch is a value the host chose, so a test can prove it (`haest-runtime` builds a whole conversation over a `MemFs` with no keys) |
| D17 | `env_passthrough = none \| core \| all`, default `core`; the `claude` profile sets `all`. Same words and `core` list as Codex's `shell_environment_policy.inherit` (whose default is `all`). With no `PATH` handed in, a program is looked up on `/usr/bin:/bin`, as `execvp` does | A key in the human's shell stays with haest unless asked; `none` still runs bash |
| D18 | The loader is a builder: `Loader::new(defaults)` then the layers in the order they win (`.xdg_dirs .user_file .file .project_file .profile .env .flags`), `.load(fs)` last. A layer not added is not read; `Loader::standard` is the CLI's chain. Built on our own registry, not `config` (config-rs) or figment: those replace only the merge (40 lines) and need serde on every settings type, where `flatten` cannot go with `deny_unknown_fields`, so a typo would pass | .NET's `ConfigurationBuilder` shape: the order is the code, readable, and a host takes only the sources it wants |
| D20 | `env`: variables passed on purpose, docker's `-e` as `-E/--env NAME=VALUE` (or `NAME` for haest's own value), repeatable; `HAEST_ENV` holds one; a file's `[env]` table. One registry row of a new kind (`Kind::Vars`), so the flag, the variable and the key come from the same place; layers add name by name, each name keeping its own source. `${NAME}` / `${NAME:-default}` are filled in at load (the `.mcp.json` expander); one with no value and no default is not passed, with a note. Handed as `RuntimeInputs::passed_env`: real processes get it over `child_env`, the sandbox's shell gets it alone. `haest settings` and `get` show names only | A key reaches a tool under the name the tool wants without `env_passthrough = all`, and is never printed |
| D21 | The 0.3 API shape: a plug point takes the value itself (`impl Into<XRef>`, a handle named after its trait, built from the value, an `Arc` or a `Box<dyn Trait>`); fields are private behind getters and `with_*` setters; a type that will grow is `#[non_exhaustive]` | A new part, field or variant is a minor release, not a break, and a host writes no `Arc` it does not need |
| D22 | New behaviour is opt-in and off by default, the `claude` profile included: wind-down, subagent budgets, durable runs, sessions and transports | D9 for features: a host that turns nothing on gets the haest it had |
| D23 | File names are the system's: no spelling layer of haest's own. The apply keeps one guard on std alone (`overlay/shown.rs`): a change whose name the directory does not list byte for byte, or that is the same file (device, inode) as another changed path, is held as a conflict. A rename by case or Unicode form alone on a disk that folds them is a documented limitation | Six review rounds spent on a layer that guessed what the disk does; std and the guard keep every file without it |
| D24 | A rare case whose handling costs more than the case is worth is a documented limitation, not a workaround: the README's "Limitations" lists them, and the item's docs say what still keeps data safe | Workarounds for exotic disks and host panics were most of the code the last review cut |
| D25 | A host's prompt template is Jinja on MiniJinja, set up as Hugging Face's text-generation-inference renders chat templates (`trim_blocks`, `lstrip_blocks`, loop controls, pycompat, `tojson`, `raise_exception`); no syntax of haest's own | The prompt reads like the `chat_template.jinja` files people already write, with first-class Rust support |
| D26 | Templating is opt-in (`haest-prompt`, feature `prompt`); the default prompt stays haest's built-in one in code. A template gets haest's built sections as values (`sections.tools`), so it reorders, wraps or drops them without copying their text | One source for the section text, and no template engine in a host that wants none |
| D27 | A model's `family`, `tier` and `generation` are catalog fields written by hand, never read out of its id; `gen-catalog.ts` keeps them across a refresh | Model ids follow no shared pattern (`claude-opus-5-5`, `gpt-6.1-sol-pro`, `qwen3.8-max-0902`) |
| D28 | Each tool writes its own line in the prompt's tool guide (`Tool::prompt_guide`); haest-core gathers the lines of the tools a call offers. The central table keyed on built-in names is gone | An MCP or host tool can say how to use it, and a renamed tool drops its line instead of keeping a wrong one |
| D29 | Extension points are Rust first: each is a Rust trait or service (`AgentHooks`, `ToolSource` and `LiveTools`, `Mod`). TypeScript is a BRIDGE: the existing TypeScript mods (`JsMod`, the loader, `ModFiles`) move, unchanged in what they offer, into `haest-bridges` behind its `js` feature (facade `bridge-js`); `haest-mods` itself links no QuickJS. Every `Mod` hook has a TypeScript twin, and a test holds the two lists equal. D41 adds the shell and HTTP bridges beside it | The maintainer, 2026-11-23: a typical CLI uses TypeScript, a host like brain uses Rust alone, a tight loop with no QuickJS linked |
| D30 | Rust mods load once per conversation and never reload. `Mods` stays a list that never changes; what changes lives INSIDE a mod (`ModFiles`' files, a mod's own `LiveTools`) | Hot reload of compiled code needs `unsafe` and a stable ABI, neither of which haest has; one immutable list keeps event order and gates simple |
| D31 | Tools change while a conversation runs: `LiveTools` (haest-core), a cheap-`Clone` shared `ToolSource` with `set_tool` (adds, or replaces by name IN PLACE), `remove_tool`, `set_enabled`, `set_tools`. Read on every model call; a call in flight finishes on the tool it started with; a disabled or removed tool is neither advertised nor callable | Tools lead the cached prompt prefix: a replacement that keeps its position, name, description and schema leaves the request bytes and the cache as they were. A real change to the `tools` array costs one cache miss from the tools on. Deferring does not avoid it for adds and removes: ToolSearch's description lists every deferred tool's name and first sentence, so only a schema change behind it is free. The provider-side answer is D40 |
| D32 | Loop events (G2 of the 2026-11-23 review): `AgentHooks` gains `after_model_call` (the answer is in history, its tools have not run; `Err` ends the turn as `before_model_call`'s does) and `before_turn_end`, asked only when a turn ends on its own (an answer without tool calls, no steering queued): `Some(text)` keeps the turn going with `text` as a user message, under the turn's caps. `Mod` gets `model.before`, `model.after` and `turn.end` through `ModHooks`, a decorator over the host's `AgentHooks`: a Rust mod gets `&mut Agent`, a TypeScript mod a JSON view and returns `{ steer }`, `{ stop }` or (`turn.end`) `{ continue }` | The loop itself becomes extensible, not only the tool calls around it; `continue` is the stop hook agents use to keep a turn going until a check passes |
| D33 | Mod files reload with `mod_reload = off \| ask \| auto` (default `off`, D22). `ModFiles` compares each file's content at every step boundary (before each model call, through the host's `Fs`; no watcher thread): a new or changed file loads there, a deleted one unloads, one that fails to load keeps its previous version and is reported. `ask` puts the human's Allow in front of each new content (a Deny keeps the old version and is not asked again for that content); `auto` loads without asking | In the real workspace the agent can write a mod file, and a mod runs with the host's trust: without `ask`, the agent's own file would rewrite the human's next message one step later |
| D34 | Mods hide tools: `Mod::disabled_tools` (TypeScript `haest.disableTool(name)` / `haest.enableTool(name)`) takes any tool, the host's, an MCP server's or a mod's, out of the advertised and callable list; `haest.removeTool(name)` drops one the mod added | "Remove" and "disable" are different promises: a removed tool is gone, a disabled one comes back as it was |
| D35 | Cache TTL is a setting: `cache_ttl = 5m \| 1h` (default `5m`, today's requests). Every breakpoint haest places carries it, one TTL per request, so Anthropic's rule (a longer TTL before a shorter one) always holds. Anthropic, and OpenRouter's Anthropic / Google path, send `ttl: "1h"`; the OpenRouter path gains the tools breakpoint the Anthropic path already has. OpenRouter gets the session id as its sticky-routing session, so a conversation stays on the provider holding its cache. OpenAI gets NO key by default: on GPT-5.6 and later `prompt_cache_key` partitions the cache rather than routing to it, so a key per session would stop one session reading what another cached (corrected 2026-11-23; the first build sent the session id, the cache policy removes it). OpenAI's retention values stay unsent until a live call proves which ones a model takes | A 1h write costs 2x the input price against 1.25x, and survives a gap of up to an hour; which pays depends on how a conversation is used, so the host picks. The one `cache_ttl` value is the first step; D39 makes it the default policy's `[cache]` setting |
| D36 | Cache accounting: `Usage` keeps cache writes per TTL (Anthropic's `cache_creation.ephemeral_5m_input_tokens` / `ephemeral_1h_input_tokens`, OpenAI's write details); `ModelPricing` gains the 1h write price; the catalog carries the cache prices of the models it lists, taken from each provider's pricing page; direct Anthropic and OpenAI calls are priced from the catalog the way the compat path already is | Keep-warm decides on these numbers, and a direct call that is never priced makes the USD caps undercount |
| D37 | Keep-warm: `Agent::warm_cache` sends the last request's prefix as a pre-warm (Anthropic: `max_tokens: 0`, not streamed, the same thinking and effort as the real request; OpenRouter: `max_tokens: 1`, since OpenRouter does not honour 0 and lets the model answer in full; measured live 2026-11-23 on `anthropic/claude-haiku-5.5`, an 8.4k prefix: 8412 tokens read, 0 written, 1 output token), books its usage on the ledger and changes no history. `keep_warm` (default off, D22) has the runtime ping an idle conversation every TTL minus a margin, only while the catalog's prices say the ping chain costs less than letting the cache expire and writing it again, and at most for `keep_warm_for`. It stops at the next turn, at close, and on a ping that wrote instead of read (reported to the human). A model or provider that cannot take a pre-warm (budget thinking, no price) does not keep warm, with a note | Pinging a 5m cache for 2h costs about 2.4x the prefix against 1.25x for one rewrite; a 1h cache with pings beats a rewrite for hours on the cheap-read models. Only the prices can say, so haest computes the break-even instead of trusting a flag |
| D38 | Compaction from mods: a Rust mod compacts through `&mut Agent` in `model.before` / `turn.end`; a TypeScript mod returns `{ compact: true }` (or `{ compact: { instructions } }`) from either, and the bridge runs the same compaction. `docs/examples/mods/` has auto-compact as a TypeScript and a Rust mod, and keep-warm as a host example | With the loop events (D32), when to compact is a policy a mod writes, not one fixed threshold |
| D39 | Cache is a POLICY, a plug point: `CachePolicy` decides, per request, which blocks carry a breakpoint and at what TTL (system, tools, the history tail, a chosen message), and, per idle tick, what to do (nothing, ping to keep warm, or compact while the cache is still warm so the next turn writes a small prefix: `compact_before_expire`). `compact_before_expire` takes its conditions and compacts only when every one holds: `min_context` (tokens, or a share of the model's context window: `120000`, `"60%"`), optional `instructions` for the summary, and always haest's own check that the cache is still warm and the summary plus the smaller next write costs less than writing the whole prefix again. Off, or a conversation below `min_context`, it does nothing. haest checks every plan before it is sent: at most 4 breakpoints, a longer TTL before a shorter one, nothing below the model's minimum cacheable length; a plan that breaks a rule is fixed and noted, never sent to fail. The default policy reads the settings: a `[cache]` table (`system`, `tools`, `history` each `off \| 5m \| 1h`, `keep_warm`, `keep_warm_for`, `compact_before_expire`) with per-model overrides under `[cache.models."<model_spec or glob>"]`. What a model CAN do is a catalog fact, overridable in `providers.json`: cache prices (D36), minimum cacheable length, 1h TTL, pre-warm, mid-conversation tool changes (D40). The CLI configures the policy through settings; a Rust host implements the trait; there is no TypeScript twin, since one request's breakpoints are no job for a mod | The maintainer, 2026-11-23: "make CACHE a function, configurable.. keep_warm, compact_before_expire... per model and fringe cache scenarios.. cache certain blocks, system prompts vs not". Hard API rules live in haest, choices in the policy, so a policy can never produce a 400 |
| D40 | Tool changes that keep the cache: on a model whose catalog row says it takes mid-conversation tool changes (Anthropic, beta `inline-tools-2026-09-15`: Claude 5.x, Opus 4.8, Haiku 5.5; not Sonnet 5), a `LiveTools` change after the first request goes out as `tool_addition` / `tool_removal` blocks in a mid-conversation `role: "system"` message, and the `tools` array stays as first sent. The change is recorded in history like any message, so every later request replays it byte for byte; a session file keeps it. Every other model gets the changed `tools` array and its one cache miss (D31) | Anthropic's docs: "The `tools` array itself never changes, so the cached prefix stays intact." It needs a message kind haest does not have yet, and a live check, since it is beta |
| D41 | Extension points are typed EVENTS, each a slot that takes hooks: `tool.before`, `tool.after`, `prompt.submit`, `system.prompt`, `model.before`, `model.after`, `turn.end`. The protocol is the TypeScript mods' own JSON (the payloads a `haest.on` handler gets, the `{ deny }` `{ input }` `{ stop }` `{ steer }` `{ continue }` `{ compact }` it returns), typed once in `haest-mods`; no second format. ONE Rust form: `impl Mod` (the default, the only kind that gets `&mut Agent`, all a Rust-only host links). The bridges live in ONE crate, `haest-bridges`, one feature each, as `haest-transport` holds its transports: `js` (the TypeScript mods, moved there from `haest-mods`, no new JS API), `shell` (`ShellHook`: a command per event, the payload on stdin, the decision on stdout, exit 2 blocks, a matcher on the tool name, and a `claude` dialect that speaks Claude Code's hook JSON so existing hook scripts run as they are), `http` (`HttpHook`: a POST per event through haest-http). The crate holds the runner the external hooks share once (timeout, `on_error = continue \| block`, matcher, encode and decode); each feature adds only its transport. `Hooks` binds external hooks per event (`.on_turn_end(HttpHook::new(url))`) and is itself a `Mod`; a config's `[[hooks.<event>]]` entries build the same. Defaults: `on_error = block` for `tool.before`, `continue` for the rest. Bindings run in order; the first deny, stop or continue wins; input rewrites chain. Every bridge is FULLY supported: every event, tests, docs, a runnable example, config; a parity test fails when an event is missing from one. A project's hooks load only in a trusted dir (D12); an `http` hook only from the human's own config (it receives every tool input); hooks run on the host, never in the agent's sandbox | The maintainer, 2026-11-23: "`end_of_turn: HttpHook::new()`... or `ShellHook::new()`... fully supported", then "don't duplicate too much, we have ts mods already" and "combine crates, so we don't re-invent or duplicate". One protocol and one runner mean a new event reaches every bridge by construction |

## Crate map

| Crate | What | Ported from |
|---|---|---|
| `haest-core` | messages, Provider/Tool/Fs/Workspace/AgentHooks contracts, the loop, guard, caps, ledger, media, effort, session log, compaction, system prompt, skills, task board, deferred tools | `core/*.ts` |
| `haest-http` | the HTTP seam: `HttpClient` (one `send`), `Request`, `Response`, `HttpError`; the default `ReqwestClient`; the one place a reqwest client is built and TLS set up. WebFetch keeps a guarded client of its own and MCP servers rmcp's, whatever client the host hands in | new (T2) |
| `haest-providers` | Anthropic Messages, OpenAI Chat Completions (any compatible endpoint), OpenAI Responses, OpenRouter routing + web search, `providers.json`, the model catalog, requests over any `HttpClient` | `providers/*.ts`, `shared/haest-models.ts` |
| `haest-workspace` | real disk + real bash; bashkit sandbox over a copy-on-write overlay, its edits a change set to review and apply | `core/workspace-*.ts`, `overlay-changes.ts`, `shell-words.ts`, `fs-walk.ts` |
| `haest-tools` | Read, Write, Edit, Glob, Grep, Bash (+ background jobs), WebFetch (+ net guard), tasks, ToolSearch, Skill, Agent tools (`SubagentHub`, `AgentDirectory`, `AgentToolOptions`), GenerateImage | `tools/*.ts`, `net-guard.ts` |
| `haest-mcp` | MCP client over `rmcp`: stdio + HTTP servers, their tools as haest tools | `core/mcp.ts` |
| `haest-code` | the code VM (`rquickjs`), the Code tool over sibling tools, the sandbox `js` command | `core/code-vm*.ts`, `tools/code*.ts` |
| `haest-mods` | mods in QuickJS: commands, prompt rewrite, tool wrapping, approvals, plan mode | `core/mods.ts` |
| `haest-ts` | the ONE TypeScript stripper (`swc` strip_types) used by Code, `js` and mods | -- |
| `haest-settings` | typed `Settings` (`Settings::bare`), `BuiltinTools` / `ContextSources` / `SkillSources` word sets; no I/O | new (D15) |
| `haest-config` | the opt-in loader: the registry (one row per setting), TOML/env/flag layers with provenance, profiles, trust, `env_passthrough`; reads the keys, `providers.json`, routing and a trusted `.mcp.json` for the runtime | new (D8, D15) |
| `haest-runtime` | the conversation built from `RuntimeInputs`: providers, workspace, tools, MCP, mods, subagents, inbox, session file, behind a `Ui` | the CLI's composition, moved (D14, D16); `host/runtime.ts` |
| `haest-prompt` | the opt-in system prompt template: Jinja on MiniJinja set up like HF chat templates, `PromptContext` (sections, model row, tools, environment), properties and a resolver chain | new (0.4, D25-D28) |
| `haest-wire` | the wire format: the envelope around a conversation's events (`v`, `sid`, `seq`, `ts`, `path`), the requests a peer sends, a replay ring with gaps, an NDJSON codec; serde only | new (0.3) |
| `haest-session` | a conversation another program watches and drives while it runs: the session driver (one turn at a time, input mid-turn steered in), its `Backend` handle, a numbered stream with catch-up and one `gap` for a slow reader, approval asks over it | new (0.3) |
| `haest-transport` | the session served to another program: one connection's rules over any line source and sink, `Peer`, and `Stdio`, `UnixSocket` and `Broker` behind their features | new (0.3) |
| `haest-cli` | the `haest` binary on `haest-config` + `haest-runtime`: flags generated from the registry, `haest settings`, print mode, `--json` events in the wire's envelope, a REPL, sessions + resume, `--stdio`, the control socket and `haest ctl`, the broker link | new (the TS haest has no CLI; its host is the broker) |
| `haest` | the one dependency a host takes; default features `providers`, `workspace`, `tools`, `mcp`, `code`, `mods`, `settings` and `reqwest` (`http` comes with them); opt-in `rmcp`, `config`, `runtime`, `testing`, `wire`, `session`, `transport`, `transport-stdio`, `transport-unix`, `transport-broker`, `prompt` | -- |
| `haest-issues` | EXPERIMENTAL, an example of how to write tools: a `Tracker` trait, a GitHub Issues backend (REST, token handed in) and a board-card backend (markdown + frontmatter files, the board's own bytes), seven Issue* tools over either; not in the facade | new; the card format from remote-claude's `project-card-file.ts` |

## Brain's plug points (from its vendored patches, `brain/web/src/rooms/agent/haest/README.md`)

| Brain patch | Rust plug point |
|---|---|
| `tool_call_start` forwarded as a loop event | `ProviderEvent::ToolCallStart` -> `LoopEvent::ToolCallStart` (in the contract) |
| `clock` in the deps (a paused run's pause doesn't count) | `SharedDeps::clock` (a `Clock`) |
| openai-compat with no SDK client: the transport is the node's gateway on files.sock | haest-http's `HttpClient` trait (default: reqwest), handed to the providers; brain's goes over its unix socket |
| no OpenRouter provider routing (the gateway decides) | routing is an option on the compat provider, off by default for a custom transport |
| workspace = BrainFs (just-bash over it, writes staged for the asker); dream runs with none | `Fs` trait + `haest-workspace` sandbox over any `Fs`; `ShutWorkspace` |
| Read: `createReadTool(intercept)` (brain media caps, `attachment:<id>`) | Read tool takes an optional intercept |
| Bash without background jobs | jobs are an option of the Bash tool |
| session-store: record types + replay only, the node writes the file | `SessionRecord` + `replay` are separate from the file writer (`SessionSink`) |
| agent tools with brain's own hub (`subagent_type`, subagent runs on the node) | haest's `SubagentHub` over brain's `SubagentParent`: `start_member` starts each helper's node run (or refuses) and hands back its `MemberRun` (model spec, caps, deps), whose `end` closes it; `AgentToolOptions` (the `subagent_type` enum, the model text). `AgentDirectory` stays for a host with no use for haest's hub |
| brain decides each run's tools and caps itself | `settings::Settings` written out in Rust, one per kind of run (no loader, nothing read) + `tools::builtin_tools_where` |

Beyond brain's patches: `ShellCommand` (haest-core) + `SandboxWorkspace::with_command`
let a host add its own commands to the sandbox shell (the CLI adds `js`). Every
sandbox has `xan` (`sandbox/xan/`, 2026-11-11): the CSV verbs TS haest's just-bash had
and brain's skills teach (headers, count, head, select, filter, sort, groupby, agg,
stats, frequency) with the part of moonblade's expressions they need; bashkit has
only a line-based `csv`.
Every row is exercised by `crates/haest/examples/notebook/` against fakes
(`cargo run -p haest --example notebook`, and under `cargo test`). A second
host that takes haest's own hub with no member hooks, shell commands and
steering instead is `crates/haest/tests/plug_in.rs`.

## Waves

| Wave | Work | Status |
|---|---|---|
| 0 | Repo, workspace, the haest-core contract (messages, provider, tool, fs, workspace, events, deps, agent shell) | done |
| 1 | T3 loop core, T4 session/compaction/context, T5 providers, T6 workspaces, T7 tools -- in parallel, one worktree each | done |
| 1b | T8 `haest` CLI on top of wave 1, verified against a real model | done |
| 2 | MCP client (`rmcp`), code VM + Code tool (`rquickjs`), mods (QuickJS), background jobs, Agent tools (subagents), Image tool | done; in the CLI too (jobs, subagents, GenerateImage, inbox turns), each checked live |
| 3 | Brain plug-in proof (custom Transport, Fs, tools, ShutWorkspace, clock, tool_call_start, in one example), parity run, README | done |
| 4 | Settings + runtime (D8-D14): `haest-settings`, `haest-runtime`, the CLI rebuilt on them, the brain example on `settle`; checked live (bare vs `--profile claude` tokens, an untrusted project file skipped, a trusted one read) | done |
| 5 | Nothing ambient (D15-D17): `haest-settings` plain values, the loader split out as `haest-config`, `RuntimeInputs` + `World` + `KeySource` + `ChildEnv`, `env_passthrough`, brain's settings written out; checked live (real bash under `none` / `core` / `all`: 5 / 14 / 109 variables, the API key only under `all`) | done |
| 6 | The loader as a builder (D18): `Loader`, one merge over ordered layers (`stack.rs`), the CLI on `Loader::standard` | done |
| 7 | Cleanup pass (/simplify, then a review agent): word lists declared once on their type; `builtin_tools_where` no longer drops `tasks` + `skill` (bug); session listing reads 16 KB a file (doubling to 256 KB) and a resume stops at its match; haest-config parses a file into one `Layer`, `provider_routing` is a registry setting; one `FILE_TOOLS`, `now_ms`, OpenRouter URL; `build()` split (`store.rs`, `runtime.rs`), MCP and mods start beside file discovery, skill roots worked out once; `Runtime::{set_model_spec, set_effort, clear, rescan_skills}`. Review fixes: everything that can fail (workspace, prompt, the new session's file) runs before an MCP server or mod starts; the session header write is awaited, so `clear` either switches or changes nothing; listing ties come in id order; a config under a file counts as absent; `word_enum!` writes the serde spelling; one session path (`session_file`); checked live (print, resume, sessions, routing source, the REPL's /model /effort /clear /skills) | done |
| 8 | `env` (D20): `-E/--env`, `HAEST_ENV`, `[env]` with `${NAME}` filled in; the sandbox shell takes variables (`SandboxWorkspace::with_env`); `.mcp.json` and `env` share one unset-variable note | done |
| 9 | Settings docs: every registry row carries an example; `docs/settings.md` is written from the registry (a test fails when it drifts, `HAEST_BLESS=1` rewrites it) and each example reads the same as a file key, a variable and a flag; `docs/environment.md` (what haest reads, what a child gets, `-E`, `${NAME}`); `docs/examples/` loaded by tests | done |
| 10 | 0.2.0: one word per concept, one path per item, typed errors, the HTTP client a plug point (`haest-http`) | done, published 2026-11-17 |
| 11 | Sessions (D22): `haest-wire`, `haest-session`, `haest-transport`; `--json` in the envelope, `--stdio`, `control_socket` + `haest ctl`, `broker_url` | done |
| 12 | Agent core (D22): durable runs (`Agent::run_durable` over a `RunStore`, `Tool::on_resume`), a subagent hub that runs a host's own members (`SubagentParent::start_member`), thresholds with wind-down, subagent budgets held on the ledger | done |
| 13 | The 0.3 API (D21); review rounds until one found no regression, leak or data loss; the spelling layer removed (D23); a last cut of boilerplate and workarounds (D24) | done, 0.3.0 |
| 14 | The system prompt as a template (D25-D28): `haest-prompt` on MiniJinja, `Tool::prompt_guide`, catalog family/tier/generation, `PromptSections`, `RuntimeInputs::with_prompt_template` behind the `prompt` feature | done, 0.4.0 |
| 15 | Live extension (D29-D41): `LiveTools`; `after_model_call` / `before_turn_end`; `Mod` loop events through `ModHooks`, tool disable / remove, the TypeScript twins; the hook protocol and `Hooks`; `haest-bridges` (`js` with `ModFiles` and `mod_reload`, `shell` with the `claude` dialect, `http`); cache TTL, per-TTL accounting and prices, `Agent::warm_cache`, `CachePolicy` with keep-warm and `compact_before_expire`, cache-safe tool changes; compaction from mods; `docs/extending.md` (every extension point and its bridges) and `docs/examples/` | in progress |

### Wave 15: what every new piece reuses

One new crate only, `haest-bridges`: a crate exists to carry a dependency a host must be able to leave out (QuickJS, a process, HTTP). Everything else goes in the crate that already owns the concept. Each piece below uses the one that exists, extended where it falls short, never a second copy:

| Need | The one place | Used by |
|---|---|---|
| A tool set that changes | `LiveTools` (haest-core) | the runtime's own tools, the TypeScript mods' tools, D40 |
| A tool-name or model-spec pattern (exact, or `prefix*`) | `matches` in haest-code, moved to haest-core for every crate | Code's `code_tools`, hook matchers, `[cache.models."..."]`. The `claude` dialect alone keeps Claude Code's regex matchers, for compatibility |
| Starting a process on the host | haest-workspace's real exec (`spawn_collect`: process group, deadline, cancel, `ChildEnv`), given stdin | `ShellHook`; haest-mcp's stdio start keeps rmcp's own transport |
| An HTTP request | haest-http's `HttpClient` | `HttpHook` |
| The event JSON | the TypeScript mods' payloads and returns, typed once in haest-mods | the `js`, `shell` and `http` bridges |
| Timeout, `on_error`, matcher, encode / decode for an external hook | one runner in haest-bridges | `shell`, `http` |
| A structured config entry (a table per key, an array of tables) | one new registry kind in haest-config, built by whichever task lands first | `[cache]` / `[cache.models."..."]`, `[[hooks.<event>]]` |
| Compacting a conversation | `haest_core::compaction::compact` | the runtime's threshold, a mod's `{ compact }`, `compact_before_expire` |
| A request that reuses the last prefix | `request_prefix.rs` | compaction's summary, `Agent::warm_cache` |

Every task's report lists what it reused and any duplicate it found; a review pass on duplication and crate boundaries runs before the wave is called done.

### Parity (2026-11-10)

Every real TS haest session on this machine (14 files in `~/.rclaude/haest/sessions`)
was replayed by both haests (TS `SessionStore.load`, Rust `session::replay`) and the
results compared field by field: **14 of 14 identical**. The run found one Rust bug
(a float one bit off, fixed in 9e3607a). The port also found and fixed four TS haest
bugs, shipped in remote-claude: the spill (2), absolute-path Glob, a custom
`type: anthropic` provider dropping its headers, and Read numbering a phantom line
after a closing newline.

### Open

| Code | Gap |
|---|---|
| G1 | The auto-mode judge is not ported |
| G2 | The sandbox overlay lives in one process: a resumed sandbox session starts from a clean overlay |
| G3 | MCP and compaction pass their tests but have not run against a live server / a real long session |
| G4 | `mv` of a directory across bashkit mounts fails |
| G5 | A PDF returned by an MCP tool stays text; it does not become media |
| G7 | The `claude` profile does not read Claude Code's agents, commands, hooks, permissions, user or plugin MCP servers, `@` imports, rules, memory or output styles (D11) |
| G8 | Durable runs keep their records in memory only (`MemoryRunStore`): a store that outlives the process (JSONL, SQLite), durable subagents and resumable `parallel`/`pipeline` flows are not built, and there is no `durable` setting yet |
| G9 | Tool costs (a price per tool call, charged when it starts and settled when it ends) are not built; adding them later breaks nothing |
| G10 | Remote subagents, the controller side (haest driving another agent through a child or WebSocket backend) and a broker server are not built |
| G11 | The cases haest leaves alone on purpose are the README's "Limitations" (D24) |

Not in this plan (the consumers, each its own job once the library holds):
remote-claude's broker-facing host in Rust (TS `host/`, 3.3k lines, 18 wire
types). brain has switched from its vendored TS copy to the crate (D6).

## Done means

- `cargo test --workspace` green, `cargo clippy --workspace --all-targets` clean.
- Every ported TS test has a Rust counterpart (behaviour, not wording).
- `haest -p "..."` runs a real multi-step task with tools against a real model,
  and `haest --resume <id>` continues it from the session file.
- A TS haest session file replays in the Rust haest.
- The notebook example compiles and runs against fakes using only plug points.

## Risks

| Code | Risk | Handling |
|---|---|---|
| R1 | bashkit is pre-1.0 | Resolved for the fs seam: `bashkit::FileSystem` is a public trait and `Bash::builder().fs(..)` takes a custom one. Pin the version; it sits behind haest's own `Fs` (D4) |
| R2 | the model catalog exists twice (TS + Rust) | Generated JSON from `shared/haest-models.ts` for now; later remote-claude reads haest's |
| R3 | parity drift while the TS haest keeps changing | Port from a named commit, record it per crate, diff upstream before each wave |
