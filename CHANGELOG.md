# Changelog

Every crate in the workspace shares one version, except haest-issues, which
is experimental and versioned on its own. Wire formats are kept compatible
with the TypeScript haest unless an entry says otherwise: the session file,
`--json` NDJSON, `providers.json`, `.mcp.json`, the settings keys and the
`HAEST_*` variables.

## Unreleased

A host and its mods can change the agent while it runs: tools that come
and go, hooks on every step of the loop, compaction from a hook, and
control over the prompt cache's lifetime. Every extension point is Rust
first; [docs/extending.md](docs/extending.md) maps them all.

### Breaking changes

- haest-mods' `Before`, `Step` and `TurnEnd`, haest-settings' `ModReload`,
  and haest-bridges' `ModCaps` are `#[non_exhaustive]`, so each can grow
  without another break: a `match` outside haest needs a `_` arm, and a
  `ModCaps` is built with `ModCaps::default().with_slice(..)`,
  `.with_memory_bytes(..)`, `.with_load_timeout(..)`.
- `Usage` gains `cache_write_1h` and `CompactionOptions` gains
  `instructions`. A struct literal that names every field no longer
  compiles: end it with `..Default::default()` (`Usage`) or
  `..CompactionOptions::new(window)`.
- Mod files moved from haest-mods to the new crate haest-bridges (feature
  `js`; the facade's `bridge-js`, on by default): `haest::mods::JsMod`,
  `ModCaps` and `ModLoadError` are now `haest::bridges::*`. haest-mods links
  no QuickJS.
- `load_mods` loads the built-ins and the host's mods only, and is no
  longer async: `load_mods(ModOptions::new(cwd, workspace), ui)`. The mod
  directories, `with_user_mods`, `with_project_mods` and `with_caps` moved
  to `haest_bridges::ModFileOptions`; the files go in as one mod,
  `ModFiles`, with `ModOptions::with_mod`.
- `Mods::names` lists the mod files as one entry, `mod files`;
  `ModFiles::names` lists their paths.
- `Message` gains the variant `ToolChange`: a `match` on it needs the arm.
- haest-config's `Config` gains the field `hooks`: a struct literal or a
  destructuring that names every field needs it (`Config::bare` and
  `Config::from(settings)` set it to none).
- The session file gains a message kind, `"role": "tool_change"`, which
  the TypeScript haest does not read: a session that changed its tools on
  a model that takes tool changes resumes in this haest only.

### Added

- haest-core: `LiveTools`, a tool set the host changes while a
  conversation runs (`set_tool`, `remove_tool`, `set_enabled`,
  `set_tools`). A `ToolSource`: the loop reads it before each model call,
  and a call already running finishes on the tool it started with.
- haest-core: `AgentHooks::after_model_call` (an answer landed, its tool
  calls have not run: steer, switch models, or end the turn) and
  `AgentHooks::before_turn_end` (the stop hook: keep a turn going until a
  check passes). `AgentHooks::on_compaction` reports every compaction,
  whoever ran it, so a host that saves history can replay it.
- haest-core: `CompactionOptions::instructions`, added to haest's own
  summary prompt.
- haest-core: the prompt cache is a policy. `CachePolicy` (a plug point)
  plans each request's breakpoints as a `CachePlan` (system, tools,
  history, chosen messages; `5m` or `1h` each) and decides what an idle
  conversation does (`IdleAction`: nothing, ping, compact). haest checks
  every plan before it goes out (at most 4 breakpoints, a longer TTL before
  a shorter one, nothing below the model's minimum) and fixes and notes a
  plan that breaks a rule instead of sending it to fail.
- The setting `[cache]`, the default policy: `system`, `tools`, `history`
  (`off`, `5m`, `1h`), `keep_warm` and `keep_warm_for` (ping an idle
  conversation before its cache expires, while the prices say it pays),
  and `compact_before_expire = { min_context, instructions }` (compact an
  idle conversation over `min_context` tokens, or a share of the window,
  while its cache is still warm and only when that costs less). Per model
  under `[cache.models."<spec or prefix*>"]`. A table setting kind in
  haest-config carries it.
- haest-core: `Agent::warm_cache` sends the last request's prefix as a
  pre-warm (Anthropic `max_tokens: 0`; OpenRouter's Anthropic models
  `max_tokens: 1`, since OpenRouter does not honour 0) and books what it
  cost. `Agent::next_cache_tick` and `Agent::cache_tick` run the idle
  policy; haest-session's driver runs them while a conversation waits.
- haest-core: `Usage::cache_write_1h` counts hour-long writes,
  `ModelPricing` has the 1h write price, and the catalog carries cache
  prices and `CacheFacts` (minimum cacheable length, 1h TTL, pre-warm,
  mid-conversation tool changes) for the models it lists, overridable in
  `providers.json`. Direct Anthropic and OpenAI calls are priced too, and
  OpenRouter's Anthropic rows carry OpenRouter's prices, so `keep_warm`
  works on them (the CLI's default model is one); a cost OpenRouter
  reports still wins over the price.
- haest-core, haest-providers: a tool change keeps the prompt cache on a
  model that takes mid-conversation tool changes (`cache.toolChanges` in
  the catalog: Anthropic's own API for Claude Opus 5.5 and Sonnet 5.5).
  The request keeps the `tools` it first sent, byte for byte, and the loop
  records each change (a tool added, removed, hidden, shown or redefined)
  as a `Message::ToolChange` right after the next user message or tool
  results; Anthropic gets it as a `system` message of `tool_addition` /
  `tool_removal` blocks, with the beta `inline-tools-2026-09-15`. A change
  after a finished turn waits for the next user message, changes between
  two model calls go out as one, a resumed session sends the same bytes,
  and compaction starts over from the tools on offer then. Every other
  model, OpenRouter's Anthropic models included, gets the current tools
  as before. `ProviderRequest::tool_baseline` carries the baseline.
- haest-core, haest-providers: a conversation's routing key
  (`AgentOptions::with_routing_key`, `Agent::set_routing_key`,
  `ProviderRequest::routing_key`; the runtime sets the session id) goes to
  OpenRouter as `session_id`, its sticky routing, so the conversation stays
  on the provider holding its cache. No other provider gets it. OpenAI's
  own API gets no cache key by default (on GPT-5.6 and later a key
  partitions the cache instead of routing to it); a policy that must keep
  caches apart sets `CachePlan::partition_key`, sent as `prompt_cache_key`.
- haest-mods: the loop events `model.before`, `model.after` and
  `turn.end`, on `Mod` and in mod files: stop the turn, steer it, keep it
  going, or compact it (`{ compact: true }` or `{ compact: { instructions } }`
  from a mod file). `ModHooks` runs them after the host's own hooks.
- haest-mods: `Mod::disabled_tools` hides any tool, whoever added it; mod
  files get `haest.disableTool`, `haest.enableTool` and `haest.removeTool`.
- haest-mods: the hook protocol, typed once: `Event` with a payload and a
  decision per event (the mod files' own JSON), `Hook<E>` for one handler,
  `Binding` for its timeout, `OnError` and tool matcher, and `Hooks`, a
  mod made of bindings. A Rust closure is a hook.
- haest-runtime: a built conversation puts its mods' loop events in the
  loop.
- haest-bridges, a new crate: the ways to hook haest from outside Rust,
  one feature each. `js` is the mod files, as `ModFiles`.
- haest-bridges: hooks in another process. `shell` is `ShellHook`, a
  command per event (the event as JSON on stdin, the decision on stdout,
  exit 2 the event's blocking answer), run on the host in its own process
  group and killed whole at its binding's timeout or an interrupt. `http`
  is `HttpHook`, a POST per event through haest-http's `HttpClient`. Both
  speak haest's own JSON (`Haest`, every event) or Claude Code's hook JSON
  (`Claude`: PreToolUse, PostToolUse, UserPromptSubmit, SessionStart and
  Stop), so Claude Code hook scripts run as they are. The facade's
  `bridge-shell` and `bridge-http`, off by default; the example
  `external_hooks` and [docs/extending.md](docs/extending.md#hooks-in-another-process).
- haest-mods: `TurnEndPayload` has `answer` (the turn's last answer, as
  `model.after` has it) and `continued` (a `turn.end` decision already
  kept this turn going), worked out once in haest-mods for every hook.
  haest now needs tokio-util 0.7.19 or later.
- haest-mods: every loop event says how full the context is: `AgentView`
  (`contextTokens`, `contextWindow` in a mod file's JSON) has
  `context_tokens`, what the last answer's request held, and
  `context_window`. haest-core: `Agent::context_tokens` and
  `Agent::context_window`, the window from the host's turn caps (the
  runtime's carry the `context_window` setting), else the catalog's facts;
  a compaction a hook asks for, and the idle tick, read it there. The example `auto_compact` and
  [docs/examples/mods/auto-compact.ts](docs/examples/mods/auto-compact.ts)
  compact as a turn ends past 60% of it.
- haest-workspace: `RealWorkspace::shell_with_stdin` hands a command its
  stdin, through the same exec as every command. haest-core:
  `workspace::MAX_OUTPUT_BYTES`, the 64 MiB a command keeps of each
  stream, which an HTTP hook's answer is held to as well.
- Mod files reload as they change: the setting `mod_reload` (`off`, the
  default; `ask`, where each new, changed or deleted file waits for the
  human's Allow; `auto`) has `ModFiles` read them again before each model
  call. A file that fails to load keeps the version that ran. Your own
  config only, never a project's.
- haest-mods: `Mods` is itself a `Mod`, so a set of mods runs inside
  another; the feature `testing` is the kit a bridge tests its events with.
- The setting `hooks`: commands and URLs bound to events from a config
  file, no Rust needed. Each `[[hooks."<event>"]]` entry is a `command` (a
  `ShellHook`) or a `url` (an `HttpHook`) with `dialect` (`haest` or
  `claude`), `matcher`, `timeout`, `on_error` and `headers` (`${NAME}`
  filled in from haest's environment); a variable or a flag holds the same
  as TOML. Every layer's entries bind after the ones before it, as one mod
  (`[hooks]`) after the built-ins and the mod files. A wrong entry fails to
  load, naming its file and its number there, never a header's value. A
  trusted project's file may bind a command, never a URL; an untrusted
  project's hooks never load. haest-bridges: `HookSettings` and
  `HookHost`, and the feature `testing` (`testing::Server`). haest-config:
  `Config::hooks`, `Loaded::hooks`. haest-runtime:
  `RuntimeInputs::with_hook_settings`, which `haest::runtime_inputs` sets;
  the facade's `runtime` turns on `bridge-shell` and `bridge-http`.
  [docs/extending.md](docs/extending.md#hooks-from-a-config-file).

### Fixed

- haest-mods: when a `tool.before` guard failed, the refusal the model
  read carried the guard's error but not its causes. It now carries both.
- haest-mods: a `tool.after` hook that replaced a tool's output dropped what
  the call spent (`cost_usd`, `child_cost_usd`), so the turn's cost, its
  caps and the ledger missed it. A rewrite now keeps the tool's spend.
- haest-workspace: a command run in a directory that does not exist
  reported `/bin/bash: No such file or directory`. It now names the
  missing directory.
- haest-cli: when a hook kept a turn going, the second answer was printed
  glued to the end of the first (`removed.No tests here.`). Each answer
  now ends its line.

## 0.4.1 (2026-11-20)

haest-issues stays at 0.3.0: nothing in it changed.

### Fixed

- haest-session: a request sent to a session just as it ended could wait
  forever instead of reading `SessionError::Closed`. A `haest -p` run whose
  conversation a peer closed during a tool call could hang this way on its
  last request, the sandbox check, without saying what it dropped.

## 0.4.0 (2026-11-20)

A host can now write haest's system prompt as a Jinja template, opt-in
through the new crate haest-prompt (feature `prompt`). Without it nothing
changes: the built-in prompt is the one 0.3.0 built, but for the tool guide
below.

### Breaking changes

- `haest_tools::tool_guide` is gone: each tool writes its own line
  (`Tool::prompt_guide`), and `haest_core::context::tool_guide(&tools)`
  gathers the lines of the tools a call offers. It takes the tools, not
  their names: `tool_guide(&names)` -> `tool_guide(&tools)`.
- The tool guide says one line per built-in tool, in the tools' order.
  "Paths may be absolute or relative to the working directory" is no longer
  in it: every file tool's schema says so.

### Added

- haest-prompt: `PromptTemplate` compiles a Jinja template once and renders
  it for each model call with a `PromptContext`: haest's built sections
  (`sections.identity`, `sections.tools`, `sections.environment`, ...), the
  model's catalog row, the tools with their guide lines, and the
  environment. MiniJinja is set up the way Hugging Face chat templates
  expect (`trim_blocks`, `lstrip_blocks`, loop controls, Python string
  methods, `tojson`, `raise_exception`). Host values come in with
  `with_property` and `with_resolver`, a chain each name goes through before
  haest's own.
- haest-runtime, feature `prompt`: `RuntimeInputs::with_prompt_template`
  renders the template before every model call, main's and each
  subagent's. A render that fails gives that call the built-in prompt, and
  the `Ui` is warned once for each new error.
- `haest` facade: feature `prompt`, `haest::prompt`.
- `Tool::prompt_guide`: the tool's line in the prompt's tool guide, default
  none. `ToolWrapper` forwards it; `ToolOverride::with_prompt_guide` and
  `without_prompt_guide` set it, and a renamed override has none until one
  is set.
- `ModelInfo::{family, tier, generation}` with their `with_*` setters: who
  makes a model (`anthropic`), which one of its family it is (`opus`,
  `flash`, `sol-pro`) and its version (`5.5`). Written in the catalog, never
  read out of a model id; all 42 bundled rows have them, and
  `scripts/gen-catalog.ts` keeps them across a refresh.
- `ModelFacts::model_info`: a spec's whole catalog row, default none;
  `input_modalities` and `reasoning` answer from it unless a host answers
  them itself.
- `PromptSections`: the system prompt's sections, built and named before
  they are joined (`build_system_prompt` is `PromptSections::new(p).join()`).

### Changed

- The bundled catalog follows the TypeScript one's 2026-11-19 sweep: Claude
  Fable 5.2 replaces 5.1, GLM-5.3 Flash serves 1048575 tokens.

### haest-issues 0.3.0 (versioned on its own)

- Builds on haest-core 0.4. Nothing else changed.

## 0.3.0 (2026-11-20)

Other programs can now watch and drive a running conversation, through three
new crates: haest-wire (the format), haest-session (one stream, one driver)
and haest-transport (stdio, a Unix socket, a broker). haest also gains
durable runs, a subagent hub that runs a host's own members, thresholds with
an opt-in wind-down, and subagent budgets; each is opt-in, every new setting
is off by default, and the `claude` profile leaves them off.

0.3.0 also finishes the API work 0.2.0 started: plug points take the value
itself instead of an `Arc<dyn Trait>`, fields sit behind getters and
`with_*` setters, and types that will grow are `#[non_exhaustive]`, so most
hosts need the edits below.

The cases haest leaves alone on purpose, and what still keeps a file safe in
each, are listed under [Limitations](README.md#limitations) in the README.

### Breaking changes

Where a parameter now takes `impl Into<XRef>`, an `Arc` of the value or an
`Arc<dyn Trait>` still goes in as it is; only what changed for such a call
is listed. A `#[non_exhaustive]` enum needs a `_` arm in a `match`; a
`#[non_exhaustive]` struct is built with `new` and `with_*` setters, and a
pattern on it ends in `..`. Both let haest add a variant or a field in a
minor release.

#### Cargo features

- `haest`: `config` no longer turns on `mcp`. `default-features = false,
  features = ["config"]` -> `["config", "mcp"]` to keep reading `.mcp.json`
  (haest-config: the new default feature `mcp`). The loader builds no MCP
  client unless asked.
- `haest`, `haest-http`, `haest-providers`, `haest-issues`: `ReqwestClient`
  and the default client are behind a new default feature, `reqwest`. With
  `default-features = false`, add `reqwest` or hand in a client
  (`ProviderRegistry::with_http`, `GithubTracker::with_http`); without one,
  every request fails and says so. A host with its own client builds no
  reqwest, rustls or ring.
- `haest-mcp`: `McpHub::connect_transport` and the `rmcp` re-export need the
  new feature `rmcp` (`haest/rmcp`), so rmcp's types are in the API only
  when asked for.

#### haest-core

- `AgentDeps::new(models, tools, prompt, cwd, workspace)` ->
  `AgentDeps::new(SharedDeps::new(models, cwd, workspace), tools, prompt)`:
  what agents share is one value, and the prompt and the cwd cannot swap
  places unseen.
- `AgentDeps`, `SharedDeps`, `ToolContext`, `AgentOptions` and
  `ResolvedModel` keep their fields private; read a part with the getter of
  its name and set it with `with_*`: `deps.shared.hooks = Arc::new(h)` ->
  `deps.with_hooks(h)`, `deps.shared.cwd` -> `deps.shared().cwd()`,
  `ctx.cwd` -> `ctx.cwd()`, `ToolContext { cwd, cancel, .. }` ->
  `ToolContext::new(cwd, ws).with_cancel(cancel)`, `opts.effort = Some(e)`
  -> `opts.with_effort(e)`, `ResolvedModel { provider, model }` ->
  `ResolvedModel::new(p, m)`. A new part is then not a breaking change.
- `AgentHooks: ToolServices` -> `AgentHooks: Send + Sync`. Tool services are
  their own slot, `SharedDeps::with_services` (default `NoServices`;
  `NoHooks` no longer implements `ToolServices`). Delete `impl ToolServices
  for Hooks {}`; hooks that publish or log go in both slots,
  `.with_hooks(h.clone()).with_services(h)`, or `.with_host(h)`.
- `Vec<Arc<dyn Tool>>` -> `Tools` and `Arc<dyn Tool>` -> `ToolRef`, in
  `ToolSource::tools` and `callable`, `deferred_tools::search_tools` and
  `advertised_tools` (takes `&Tools`); `impl ToolSource for Vec<Arc<dyn
  Tool>>` -> `Tools`. `vec![Arc::new(Read) as Arc<dyn Tool>]` ->
  `Tools::new().with(Read)`. A `Tools` list holds one tool per name.
- `Tools::get` and `Tools::contains` take a tool's name, shadowing the
  slice's: `tools.get(0)` -> `tools.first()` or `tools[0]`.
- `Workspace::fs() -> Arc<dyn Fs>` -> `FsRef`, `Workspace::changes()` ->
  `Option<ChangeSetRef>`, and `ShellCall::fs` is an `FsRef`: an implementor
  returns `self.fs.clone().into()`.
- `OutputChunks` is a struct, not an `Arc<dyn Fn>` alias: `Arc::new(move |t:
  &str| ..)` -> `OutputChunks::from(move |t: &str| ..)`.
- `Fs::mkdir(path, recursive)` -> `create_dir(path)` /
  `create_dir_all(path)`, `Fs::remove(path, recursive)` -> `remove(path)` /
  `remove_all(path)`. An `Fs` implements `create_dir` and `remove_all`
  itself (`fs::remove_tree(self, path)` walks a tree), so no default deletes
  a tree past a store's own rules.
- `FsStat { kind, size, modified }` -> `FsStat::new(kind,
  size).with_modified(at)`, `DirEntry { name, kind }` ->
  `DirEntry::new(name, kind)` (both `#[non_exhaustive]`).
- `Arc<Mutex<Ledger>>` -> `Ledger`, a cheap clone whose methods take
  `&self`: `ledger.lock().unwrap().usd()` -> `ledger.usd()`,
  `Ledger::new(caps, Arc::new(h))` -> `Ledger::new(caps, h)`,
  `ledger.with_remedies(r)` -> `ledger.set_remedies(r)`;
  `SharedDeps::with_ledger` takes a `Ledger` and `ledger()` returns
  `Option<&Ledger>`.
- `Arc<TaskBoard>` -> `TaskBoard` (clones are one board);
  `TaskBoard::with_clock(f, clock)` -> `TaskBoard::new_with_clock(f,
  clock)`; `board.create(task)` returns `Result<Task, TaskError>`, refusing
  with the new `TaskError::IdsExhausted` instead of handing out an id twice.
- `Arc<ToolDeferral>` -> `ToolDeferral` (clones share what is loaded).
- `MemFs::with_files(files)` -> `MemFs::from_files(files)`; `MemFs` clones
  are one file system, so `Arc::new(MemFs::new())` -> `MemFs::new()`.
- `testing::ScriptedProvider::new` returns the provider (was
  `Arc<ScriptedProvider>`); `&Arc<ScriptedProvider>` -> `&ScriptedProvider`,
  `resolver(&self)`.
- `JsonlSessionFile::with_error_handler(..)` ->
  `JsonlSessionFile::new_with_error_handler(..)`; both constructors take
  `fs: impl Into<FsRef>`, and `create(cid, "/project", ms)` -> `create(cid,
  Path::new("/project"), ms)`.
- `SessionRecord::header(sid, cid, cwd, ms)` ->
  `SessionRecord::from(SessionHeader { session_id, conversation_id, cwd,
  created_ms })`, so the two ids cannot trade places.
- `SessionRecord::Config::effort` and `ReplayedSession::effort`:
  `Option<Option<Effort>>` -> `EffortChange` (`Some(Some(e))` -> `To(e)`,
  `Some(None)` -> `ModelDefault`, `None` -> `Unchanged`; `.unwrap_or(cur)`
  -> `.applied_to(cur)`). The session file's JSON is unchanged.
- `SessionRecord` gains `Run`, `Turn` and `RunEnd` (durable runs;
  `session::replay` skips them) and is `#[non_exhaustive]`.
- `DiskSkills` is gone: `Skills` is the one set of SKILL.md skills, a list
  the host replaces after a rescan: `DiskSkills::new(skills, fs)` -> `let s
  = Skills::new(fs); s.replace(list);`.
- `TurnOutcome` is `#[non_exhaustive]` and `#[must_use]`, and gains `limit:
  Option<Limit>`, the cap that stopped the turn: `TurnOutcome::with_error(r,
  why)` -> `TurnOutcome::error(r, why)`, a literal ->
  `TurnOutcome::new(stop_reason)` (`with_limit`).
- `ProviderRequest`, `ExecOptions` and `ShellCall` are `#[non_exhaustive]`:
  `ProviderRequest { .. }` -> `ProviderRequest::new(model,
  messages).with_system(s).with_cancel(c)` (`with_tools`, `with_effort`,
  `with_max_tokens`, `with_web_search`), `ExecOptions { cwd, cancel, timeout
  }` -> `ExecOptions::new(cwd, timeout).with_cancel(cancel)`, `ShellCall {
  .. }` -> `ShellCall::new(args, cwd, fs, ends_at)` (`with_env`,
  `with_stdin`, `with_cancel`).
- Rows a host builds are `#[non_exhaustive]` with constructors (their JSON
  is unchanged): `SkillCard::new(name, description)` (`with_when`,
  `with_origin`), `InlineSkill::new(card, body)`, `Reasoning::new(efforts)`
  (`with_mandatory`), `Change::new(path, kind)` (`with_conflict`),
  `FailedChange::new(change, code, reason)`, `ModelPricing::new(input,
  cached_input, output)` (`with_cache_write`), `ModelInfo::new(spec,
  context_window)` (`with_label`, `with_input`, `with_reasoning`,
  `with_pricing`), `ModelGroup::new(label, models)`.
- `TurnCaps` gains `context_window` and `subagent`, `SubagentCaps` gains
  `budgets`: a literal adds them or ends in `..`.
- `CompactionError::Failed(reason, usage)` -> `CompactionError::Failed {
  reason, usage }`.
- `total.add(&u)` -> `total += &u` (`Usage` also implements `Sum`).
- `tool::opt_bool(input, k)` returns `Result<bool, ToolError>`, like
  `opt_num` and `opt_str`: add `?`.
- `ChildEnv::new([("K".into(), v)])` -> `ChildEnv::new([("K", v)])` (pairs
  of anything `Into<String>`; a pair spelled with `.into()` no longer
  infers), and `base.with(&over)` -> `base.overlaid(&over)`.
- `__private::NUDGE_SHARE` -> `threshold::NUDGE_SHARE`.
- Now `#[non_exhaustive]`: `LoopEvent`, `ProviderEvent`, `StopReason`,
  `ToolError`, `InnerCallEvent`, `ModelError`, `CompactionError`,
  `MediaError`, `TaskError`, `AssistantPart`, `MediaKind`, `InputModality`,
  `Effort`, `WebSearchEngine`, `WorkspaceMode`, `OpenRouterEffort`,
  `ApplyFailureCode`, `ConversationCap`, `SkillSource`.

#### haest-http

- `Request` and `Response` are `#[non_exhaustive]`: `Response { status,
  headers, body }` -> `Response::new(status, body).with_headers(headers)`,
  `Request { body, ..Request::post(url) }` ->
  `Request::post(url).with_body(body)`.
- Now `#[non_exhaustive]`: `Method`, `HttpError`.

#### haest-providers

- `ProviderRegistry::new(Arc::new(keys), custom, RegistryOptions { http,
  routing, .. })` ->
  `ProviderRegistry::new(keys).with_custom(custom).with_http(client)`, then
  `.with_routing(scopes)` when there are any (also `with_retry`,
  `with_headers_timeout`, `with_app_title`, `without_app_title`,
  `with_app_url`). `RegistryOptions` is gone: one setter per option.
- `AnthropicOptions`, `CompatOptions` and `ResponsesOptions` keep their
  fields private: `opts.base_url = Some(u)` -> `opts.with_base_url(u)`,
  `opts.flavor = f` -> `opts.with_flavor(f)`, `opts.base_url.as_deref()` ->
  `opts.base_url()`. Only the base URL reads back; a host keeps the key, the
  headers and the rest of what it set.
- `haest_providers::routing::parse_routing` ->
  `haest_providers::parse_routing`, and the same for `RoutingRule`,
  `RoutingOverride`, `ProviderRouting`, `ProjectRouting` and
  `RoutingPriority`; the `routing` module is private.
- `CustomProviders` is a type of its own (was `BTreeMap<String, Value>`):
  `CustomProviders::from([(n, json)])` -> `serde_json::from_value(json!({ n:
  json }))?` or `CustomProviders::new().with_entry(n, config)`,
  `custom.contains_key(n)` -> `custom.contains(n)`, `custom.keys()` ->
  `custom.names()`, `CustomProviderConfig::from_json(&custom[n])` ->
  `custom.get(n)`.
- `CustomProviderConfig::resolved_key(&keys) -> String` -> `key(&keys) ->
  Option<String>`: None where it returned `"none"`.
- `testing::FakeHttp::new` returns a `FakeHttp` (clones share the script and
  the record), and `FakeReply::header(n, v)` -> `FakeReply::with_header(n,
  v)`.
- Now `#[non_exhaustive]`: `CompatFlavor`, `Dialect`, `RoutingPriority`,
  `EntryProblem`, `ProvidersFileError`, `testing::FakeReply`,
  `testing::FakeEnd`.

#### haest-tools

- Every tool list is `Tools` (was `Vec<Arc<dyn Tool>>`): `builtin_tools`,
  `builtin_tools_where`, `agent_tools`, `job_tools`, `task_tools`,
  `SubagentHub::tools_for`, `SubagentParent::base_tools` and
  `subagent_tools`. `SubagentParent::tool_source` takes `Tools` and returns
  a `ToolSourceRef` (`Arc::new(s)` -> `s.into()`), and
  `SubagentParent::system_prompt` takes `&[ToolRef]`.
- `builtin_tools_where(|w| set.has(w), o)` -> `builtin_tools_where(|t|
  set.has(t.as_str()), o)`, `builtin_word(n) == Some("tasks")` ->
  `BuiltinTool::of(n) == Some(BuiltinTool::Tasks)`, `MissingOption::word` ->
  `MissingOption::tool` (a `BuiltinTool`).
- `BuiltinOptions`, `BashOptions`, `GenerateImageOptions` and
  `SubagentTypes` keep their fields private: `BuiltinOptions { tasks:
  Some(Arc::new(b)), web_fetch: None, .. }` ->
  `BuiltinOptions::new().with_tasks(b).without_web_fetch()`, `BashOptions {
  jobs: Some(h), .. }` -> `BashOptions::default().with_jobs(h)`,
  `GenerateImageOptions { base_url, http, ..GenerateImageOptions::new(key)
  }` -> `GenerateImageOptions::new(key).with_base_url(u).with_http(c)`,
  `SubagentTypes { names, description, default }` ->
  `SubagentTypes::new(default).with_kinds(names).with_description(d)`.
- `ReadTool::new(Some(Arc::new(i)))` -> `ReadTool::new().with_intercept(i)`;
  `WriteTool`, `EditTool`, `GrepTool` and `GlobTool` are
  `#[non_exhaustive]`: `WriteTool` -> `WriteTool::new()`.
- `skill_tool(opts)` -> `SkillTool::new(opts)`; `SkillToolOptions::new(list,
  Arc::new(fs))` -> `SkillToolOptions::new(skills)` with `skills` a
  `Skills`, `SkillToolOptions::over(Arc::new(set))` ->
  `SkillToolOptions::new(set)`, `.with_name_enum()` ->
  `.with_name_enum(true)`.
- `task_tools(Arc::new(board))` -> `task_tools(board)`,
  `job_tools(Arc::new(hub))` -> `job_tools(hub)`.
- `JobHub` is a cheap handle and only dropping its last clone kills the jobs
  still running: `Arc<JobHub>` -> `JobHub`,
  `JobHub::with_events(Arc::new(tx), Arc::new(ev))` -> `let hub =
  JobHub::new(tx); hub.set_events(ev);`, `jobs.start(ws, cmd, desc, cwd)` ->
  `jobs.start(NewJob::new(cmd).with_description(desc), &ctx)`.
- `JobEvents::on_output(job, chunk, done)` -> `on_output(job,
  JobOutput::Chunk(chunk) | JobOutput::Done)`; `Job::state()` returns a
  `JobState` (it implements `Display`).
- `SubagentHub::new(Arc::new(parent), caps)` returned an `Arc<SubagentHub>`
  -> `SubagentHub::new(parent, caps)` returns the hub, a cheap handle;
  `AgentDirectory::spawn` and `SubagentHub::spawn` take
  `&CancellationToken`.
- `AgentDirectory::deliver("lead", "worker", "go")` (and
  `SubagentHub::deliver`) ->
  `deliver(AgentMessage::new(Sender::Agent("lead".into()), "worker",
  "go"))`: no three strings in a row.
- `agent_tools(dir, me, can_spawn)` and `agent_tools_with(dir, me,
  can_spawn, opts)` -> `agent_tools(dir, me,
  opts.with_can_spawn(can_spawn))`. `AgentToolOptions` keeps its fields
  private: `AgentToolOptions { types: Some(t), model_description: Some(d),
  ..Default::default() }` ->
  `AgentToolOptions::default().with_types(t).with_model_description(d)`
  (`with_budget_caps`, `with_can_spawn`).
- `SubagentHub`'s `spawn`, `deliver` and `stop` are its `AgentDirectory`
  impl's: `use haest::tools::AgentDirectory;` to call them.
  `hub.answers_due() == 0` -> `hub.idle()` (or `settled(timeout)`).
- `subagent_note(name, parent, description, caps)` ->
  `subagent_note(&member_start)` (a `MemberStart`).
- `SpawnInput` and `SpawnOutcome` are `#[non_exhaustive]`: `SpawnInput {
  prompt, description, .. }` ->
  `SpawnInput::new(prompt).with_description(d)` (`with_name`,
  `with_model_spec`, `with_subagent_type`, `with_budget`,
  `with_background(bool)`), `SpawnOutcome { text, cost_usd }` ->
  `SpawnOutcome::new(text, cost_usd)`.
- `AgentError` no longer implements `Eq` (its new `OverBudget` holds USD as
  `f64`).
- `media_output(label, bytes, media_type, ctx)` -> `media_output(bytes,
  media_type, label, ctx)`, the order `media_from_bytes` takes. Both text
  arguments are `&str`, so an old call still compiles: swap them by hand.
- `assert_public_url(&Url::parse(s)?, &dns)` -> `assert_public_url(s,
  &dns)`; text that is not a URL is the new `UrlRefused::Invalid`.
- `DeferredTools::deferral()` returns `&ToolDeferral` (was
  `&Arc<ToolDeferral>`).
- The Image API helpers are no longer public (see Removed): call the Image
  API through `GenerateImageTool`.
- Now `#[non_exhaustive]`: `AgentError`, `SubagentEvent`, `JobStatus`,
  `JobError`, `UrlRefused`, `Job`, `Taken`, `MissingOption`.

#### haest-workspace

- `RealWorkspace::fs` and `SandboxWorkspace::fs` return an `FsRef`,
  `SandboxWorkspace::changes` an `Option<ChangeSetRef>`, as `Workspace` now
  has them.
- `SandboxWorkspace::open` fails with an `OpenSandboxError` that names the
  dir (was `io::Error`; `?` still turns it into an `io::Error` of the same
  kind).
- `RealWorkspace::with_fs` is gone (the real workspace is always over
  `RealFs`), and `sandbox.overlay_changes()` -> `sandbox.changes()`.

#### haest-code

- `code_tool_names(&[Arc<dyn Tool>], &[String])` ->
  `code_tool_names(&[ToolRef], &[impl AsRef<str>])`; `code_callable` takes
  the names as `&[impl AsRef<str>]`.
- `JsEnv` keeps its fields private: `JsEnv { fs: Arc::new(mem), cwd, env,
  stdin, cancel, ends_at }` -> `JsEnv::new(mem,
  cwd).with_env(env).with_ends_at(at)` (`with_stdin`, `with_cancel`), and
  `with_env` takes a `ChildEnv` (was a `BTreeMap`).
- `VmSpec` keeps its fields private and a native function is a plain async
  closure: `spec.native_fns.insert(name, Arc::new(|a| async { ..
  }.boxed()))` -> `VmSpec::new().with_native_fn(name, |a| async { .. })`
  (`with_prelude`, `with_value`, `with_modules`, `with_filename`,
  `with_caps`). `NativeFn` and `NativeFuture` are gone.
- `run_in_vm(code, &spec, caps, &cancel, Some(&|b| log(b)))` ->
  `run_in_vm(code, &spec.with_caps(caps), &cancel, |b| log(b))`, and `None`
  -> `|_| {}`.
- `sandbox_commands()` returns `Vec<ShellCommandRef>`.
- Now `#[non_exhaustive]`: `VmRun`, `VmBeat`, `VmStop`.

#### haest-mcp

- `McpHub::with_env(env).connect(&servers, &|line| ..)` ->
  `McpHub::new().with_env(env).with_log(|line| ..).connect(&servers)`: the
  log is set once.
- `hub.connect_transport(name, io, &|_| {})` -> `hub.connect_transport(name,
  io)` (feature `rmcp`).
- `McpHub::tools` returns `Tools`.
- `StdioTransport::with_env(k, v)` -> `with_env_var(k, v)`: it adds one
  variable, and `with_env` means a whole environment everywhere else.
- `McpFailure` keeps its fields private: `failure.name` -> `failure.name()`,
  `report(&*failure.error)` -> `report(failure.error())`.
- `load_project_mcp_config(fs, cwd, &|name| ..)` ->
  `load_project_mcp_config(fs, cwd, |name: &str| ..)`.
- `McpServerConfig`, `HttpTransport`, `StdioTransport` and `ProjectMcp` are
  `#[non_exhaustive]`: `HttpTransport { url, headers }` ->
  `McpTransport::http(url).with_header(..)`, `ProjectMcp { servers,
  ..Default::default() }` -> `let mut p = ProjectMcp::default(); p.servers =
  servers;`.
- Now `#[non_exhaustive]`: `McpTransport`, `McpStartError`,
  `McpConfigError`.

#### haest-mods

- `Mod::tools`, `Mods::tools` and `ModTools::wrap` take and return `Tools`.
- `Mods::add` is gone and a list never changes once built: `let mut m =
  Mods::new(env); m.add(Arc::new(NoDotEnv)); let m = Arc::new(m);` -> `let m
  = Mods::new(env, [ModRef::new(NoDotEnv)]);`. Hold `Mods` (a cheap clone)
  where `Arc<Mods>` was: `ModTools::new(source, mods)`,
  `ModTools::wrap(&mods, tools)`, `ModSystemPrompt::new(prompt, mods)`. A
  host's own mod for `load_mods` goes in with `ModOptions::with_mod`.
- `ModEnv` and `ModOptions` keep their fields private: `env.workspace` ->
  `env.workspace()`, `cfg.user_mods = true; cfg.auto = Some(Arc::new(j));`
  -> `ModOptions::new(cwd, mode,
  dir).with_user_mods(true).with_approval(ApprovalMod::new().with_auto(j))`
  (`with_project_mods`, `with_approvals`, `with_caps`; the plan mod's
  listener and tools go in as
  `with_plan(PlanMod::new().with_listener(f).with_planning_tools(n))`, the
  shell tools as `ApprovalMod::with_shell_tools`). `ModOptions` is no longer
  `Clone`.
- `ApprovalMod::new(Some(Arc::new(judge)))` ->
  `ApprovalMod::new().with_auto(judge)`; `PlanMod::new(Some(Arc::new(f)))`
  -> `PlanMod::new().with_listener(f)` (`PlanListener` is gone).
- `gate.with_lines(f)` -> `gate.mod_ui(f)`: `with_*` is only ever a setter.
- `ModLine` gains `Warn` and is `#[non_exhaustive]`: `ModLine::Info(t) =>
  show(t)` -> `other => show(other.text())`.
- `ApprovalRequest` is `#[non_exhaustive]`: `ApprovalRequest { tool_name,
  description, input_preview, tool_use_id: None, key: Some(k) }` ->
  `ApprovalRequest::new(tool_name,
  description).with_input_preview(p).with_key(k)` (`with_tool_use_id`).
- Now `#[non_exhaustive]`: `ModLoadError`, `CommandOutcome`, `BashVerdict`,
  `ApprovalAnswer`.

#### haest-settings

- `CapSettings::max_turn_minutes: f64` -> `max_turn_time: Duration`
  (`caps.max_turn_minutes = 1.5` -> `caps.max_turn_time =
  Duration::from_secs(90)`). The setting keeps its name and reads minutes.
- `SubagentCapSettings` is gone: `Settings::subagents` is haest-core's
  `SubagentCaps`, with the same field names except `minutes: u64` -> `time:
  Duration` (the setting still reads minutes), and
  `settings.subagent_caps()` -> `settings.subagents`.
- `Settings` gains `wind_down_at`, and `subagents` gains `budgets`: a
  literal adds them or ends in `..`.

#### haest-config

- `Loader::env`, `flags`, `standard` and `xdg_dirs` take pairs by value:
  `Loader::standard(defaults, &env, &flags)` -> `Loader::standard(defaults,
  env, flags)` (a `&HashMap` still goes in; a `&Vec` of pairs as
  `.iter().cloned()`).
- `Loaded::keys` is a `Keys` (was `HashMap<String, String>`):
  `loaded.keys["K"]` -> `loaded.keys.get("K")`. `Loaded` is
  `#[non_exhaustive]`: a pattern names its fields then `..`.
- `ConfigError::Invalid { at, why }` -> `Invalid { at, key, why }`, and an
  unknown setting is its own `ConfigError::UnknownSetting { at, key }`, so a
  settings screen can point at the setting.
- `Config` gains `event_ring`, `stdio`, `control_socket`, `control_dir`,
  `broker_url` and `broker_access`: a literal ends in `..`.
- Now `#[non_exhaustive]`: `ConfigError`, `SetError`, `Kind`, `Source`,
  `Scope`, `EnvPassthrough`.

#### haest-runtime

- The assembly is the runtime's own: `RuntimeParent`, `WrappedTools`,
  `MainTools`, `RuntimePrompt`, `AgentPrompt`, `InboxSender`, `NoImageKey`,
  `own_tools`, `image_options`, `instruction_options`, `skill_options`,
  `workspace_for`, `jobs_for`, `connect_mcp`, `mods_for`, `inbox` and
  `session_usd` are private. Call `build` and read the parts on the
  `Runtime` (`mods_for(..)` -> `rt.mods()`); a host that owns some parts
  builds its own `AgentDeps`.
- `build(inputs, Arc::new(ui), prompter, opts)` -> `build(inputs, ui,
  prompter, opts)`, and `Runtime<U>` needs a sized `U`; a host that keeps
  its UI too hands in an `Arc` of it.
- `Runtime` keeps its fields private: `rt.hooks.flush()` ->
  `rt.hooks().flush()`, `rt.jobs.clone()` -> `rt.jobs().cloned()`,
  `rt.gate.revoke(t)` -> `rt.gate().revoke(t)`, `*rt.skills.lock().unwrap()
  = found` -> `rt.skills().replace(found)`, `rt.inbox` -> `rt.inbox_mut()`.
  Its services are handles: `Arc<RuntimeHooks>`, `Arc<McpHub>`,
  `Arc<SubagentHub>`, `Arc<JobHub>`, `Arc<TaskBoard>`, `Arc<Mods>` -> the
  value.
- `RuntimeInputs` keeps its fields private: `RuntimeInputs { settings,
  world, .. }` -> `RuntimeInputs::new(settings, world)`, `inputs.keys =
  Arc::new(k)` -> `.with_keys(k)`, likewise `with_http`, `with_mcp_servers`,
  `with_providers`, `with_passed_env`, `with_routing(scopes)` and
  `with_world`; `keys()` is a `KeySourceRef`, `http()` an `HttpClientRef`. A
  result of the facade's `runtime_inputs` is changed the same way.
- `World::Disk { child_env }` -> `World::disk(child_env)`,
  `World::Fs(Arc::new(fs))` -> `World::files(fs)`; `World::fs()` returns
  `&FsRef`; `world.child_env(&ChildEnv::default()).is_some()` ->
  `world.starts_processes()`, `world.child_env(&passed)` ->
  `world.child_env().map(|e| e.overlaid(&passed))`.
- `RuntimeHooks` keeps its fields private (`hooks.settings` ->
  `hooks.settings()`); `hooks.ui` and `hooks.catalog` are gone
  (`Catalog::builtin()`), and `new`, `store`, `remedies`, `window` and
  `compaction_for` are private. `hooks.compact_now(&mut agent, &cancel,
  false)` -> `rt.compact(&cancel)`, `hooks.append(record)` ->
  `hooks.append(&record)`.
- `SessionError` -> `SessionFileError` and `BuildError::Session(..)` ->
  `BuildError::SessionFile(..)` (haest-session has a `SessionError` of its
  own); `Runtime::clear` returns it.
- `BuildOptions { resume, model_spec_given: true, effort_given: false }` ->
  `BuildOptions { resume, on_resume: OnResume { model: Prefer::Settings,
  effort: Prefer::Saved } }`.
- `WorkspaceError::Sandbox` holds an `OpenSandboxError` (was `io::Error`;
  `e.kind()` still works). `SessionInfo::cwd` is a `PathBuf` (was `String`).
- `InboxQueue::wait()` returns whether anything can still arrive:
  `q.wait().await;` -> `if !q.wait().await { .. }`.
- Now `#[non_exhaustive]`: `BuildError`, `SessionFileError`,
  `WorkspaceError`, `ShutDown`, `SessionInfo`, `Resume`.

#### haest-ts

- `StripError` is `#[non_exhaustive]`.

#### haest-issues 0.2.0 (versioned on its own)

- `CardTracker`'s and `GithubTracker`'s methods are their `Tracker` impl's:
  `use haest_issues::Tracker;` to call them.
- `issue_tools(Arc::new(t))` -> `issue_tools(t)`,
  `issue_tools_named(Arc::new(t), "Card")` -> `issue_tools_named("Card",
  t)`; both return `Tools`.
- `CardTracker::new(Arc::new(fs), dir)` -> `CardTracker::new(dir, fs)`, and
  `Arc<CardTracker>` -> `CardTracker` (clones share one write lock).
- `TrackerInfo` is `#[non_exhaustive]`: `TrackerInfo { name, id_hint,
  statuses, .. }` -> `TrackerInfo::new(name, id_hint, statuses)`
  (`with_open_statuses`, `with_priorities`, `with_relations`).
- Now `#[non_exhaustive]`: `TrackerError`, `Relation`.

#### Behaviour a host must act on

- `compaction::compact` books each summary call on the agent's ledger as it
  comes back: delete a host's own `ledger.charge(&res.usage)` after it, or
  the calls count twice.
- A providers.json entry with a key it does not have (`baseUrl`,
  `_comment`), or a `baseURL`, `apiKey` or `apiKeyEnv` that is not a string
  (`null` included), fails when its provider is resolved
  (`EntryProblem::UnknownKeys`, `NotAString`), and nothing is sent.
- A configured URL (a provider's base URL, an MCP server's URL,
  GenerateImage's base URL, a GitHub API root, `broker_url`) with an `@`
  after its host, or with no host, is refused before anything connects.
  Percent-encode the password (`/` as `%2F`, `@` as `%40`).
- A resume of a session another run holds fails with
  `SessionFileError::InUse`. A run holds its session through `<id>.lock`
  beside the session file, which the system releases with the process.
- `GithubTracker` acts only on issues in its own repository: another
  repository's id is `Invalid`, and a search is plain words in this
  repository.
- `haest --json`: every line is in the wire's envelope (`v`, `sid`, `seq`,
  `ts`, `path`) with its old fields beside it; a subagent's life comes as
  `subagent_started`, its events and `subagent_stopped` under `["main",
  "<id>"]`, in place of `notice` lines with its name.
- A tool flag the model sends as `"yes"` or `1` is an input error the model
  reads (was a silent false); `"True"` and `"FALSE"` read as the flag.
- A foreground subagent spawn needs a tokio runtime, as a background one
  did.
- An apply, and Edit, Write and GenerateImage in the real workspace, write
  over a file by renaming a new one onto it (they wrote in place). The file
  keeps its mode, owner and group, not its extended attributes or ACLs, and
  a process that has the old file open keeps the old one. A file whose owner
  or group the run may not give (another user's, a group the run is not in)
  is not written: the write fails and the file stays as it was. A
  hard-linked file, a symlink, a FIFO and a device are still written in
  place.
- On a disk that reads two names as one, an apply holds a rename by case or
  Unicode form alone (`mv README.md readme.md`) as a conflict: make it on
  disk by hand. The change list shows it as no conflict until an apply skips
  it.
- The `haest` command asks for approval only where the question can be seen:
  stdin and stderr must be terminals (with `--json`, stdout must not be a
  regular file); otherwise an ask is refused at once.

### Added

- Sessions (facade features `wire`, `session`, `transport`,
  `transport-stdio`, `transport-unix`, `transport-broker`, none in
  `default`): haest-wire's `Envelope`, `Outbound`, `Inbound`, `Ring` and
  `LineDecoder`; haest-session's `Hub`, `start` and `SessionHandle` (one
  turn at a time, input mid-turn steered in, catch-up with gaps), the
  `Backend` and `Driven` seams, and `SessionPrompter` for approval asks over
  the session; haest-transport's `serve`, `Stdio`, `UnixSocket` (with
  `running_sessions` and `connect_session`) and `Broker`. With `runtime`,
  `impl Driven for Runtime` and `HubUi`. Examples `wire`, `session`,
  `stdio`, `control_socket`, `broker`.
- The `haest` command: `--stdio` (`HAEST_STDIO`), `control_socket = "watch"
  | "command"` and `control_dir`, `haest ctl` (`list`, `inspect`, `events`,
  `send`, `interrupt`), `broker_url` and `broker_access` with the token in
  `HAEST_BROKER_TOKEN`, and `event_ring` (1000). The control and broker
  settings come only from the user's own file, a variable or a flag, never
  from a project's file.
- Durable runs: `Agent::run_durable(&key, input, sink)` over a
  `run::RunStore` (`SharedDeps::with_run_store`, `run::MemoryRunStore`): a
  finished run returns its stored result, an unfinished one resumes.
  `Tool::on_resume` (`Rerun`, `Report`, `Lookup`), `ToolContext::attempt`
  and `effect_key`, `AgentHooks::before_effects`. Example `durable_run`.
- A subagent hub that runs a host's own members:
  `SubagentParent::start_member`, `Member`, `MemberRun`, `MemberStart`,
  `MemberEnd`, `MemberCaps`, `Ending`, `Spend`; messages from the person
  (`AgentMessage`, `Sender::Person`, `SubagentEvent::Message`),
  `Inbox::submit_answer`, `SubagentHub::idle` and `settled`,
  `SubagentParent::subagent_note`.
- Subagent budgets (`subagent_budgets`, `SubagentCaps::budgets`): the Agent
  tool takes a `budget {usd, minutes}` (`SubagentBudget`) that only tightens
  the caps, held on the ledger while the member runs
  (`AgentError::OverBudget`, `OverTime`).
- Ledger holds: `Ledger::reserve`, `available` and `held`, `ledger::Hold`
  (gives its USD back when dropped) and `NotAvailable`.
- Thresholds (`threshold`): a `Meter` crossing a `Mark` fires an `Action`
  (remind the model, warn the human, wind down, compact, stop). The guard's
  nudge, the agent-turn gate's nudge, the ledger's warning and compaction
  are default thresholds with their old numbers.
  `SharedDeps::with_thresholds`, `Ledger::set_thresholds`,
  `AgentTurnGate::with_thresholds`, `AgentHooks::on_warning` (the runtime
  passes it to `Ui::warn`). Example `threshold`.
- Wind-down (`wind_down_at`, `HAEST_WIND_DOWN_AT`, a share from 0 to 1, 0 =
  off): past the mark, a tool that starts new work refuses and the model is
  told to conclude. `WindDown::at`, `Thresholds::wind_down`,
  `ToolContext::wind_down()`, `AgentError::WindingDown`. Example
  `wind_down`.
- `ToolContext::turn_time_left()`, and `remedy::Limit::{SubagentSteps,
  SubagentTime, SubagentUsd}` with `Limit::setting()`.
- Plug-point handles named after their trait (`ToolRef`, `FsRef`,
  `WorkspaceRef`, `ProviderRef`, `HttpClientRef`, ...), built from the
  value, an `Arc`, an `Arc<dyn Trait>` or a `Box<dyn Trait>`;
  `SharedDeps::with_host` fills the hooks, services, facts, caps and clock
  slots with one value. `SystemPrompt` is implemented for `&'static str` and
  closures.
- For tool and provider authors: `ToolWrapper`, `ToolOverride`,
  `ToolError::message` and `map_message`, `tool::input`, `req_num`,
  `opt_count`, `ExecResult::ok` and `failed`, `answer_stream` with
  `provider::Deltas`, `ResolvedModel::complete` and `complete_streaming`; a
  `ProviderRef` and a `ResolvedModel` are each a `ModelResolver`.
- `Tools::get`, `contains`, `remove` and `with_all`; `skills::Skills` (a
  skill list a rescan replaces); `ledger::NoLedgerHooks`, with default
  bodies for `LedgerHooks`, `Clock::now_ms` and `TurnCapsSource::turn_caps`;
  `newest::Newest<T>`.
- `Fs::try_lock` and `FsLock`, `Fs::write_whole` (a write that replaces a
  file whole or not at all).
- `CompactionOptions::new(window)`, `compaction::COMPACT_SHARE` and
  `KEEP_RECENT`, `threshold::WARN_SHARE`.
- haest-http: `Request::with_body`, `Response::with_headers`,
  `redacted_url`, `check_endpoint_url`; `Method` implements `Display` and
  `FromStr`; feature `http` converts to and from the `http` crate.
- haest-providers: `ProviderRegistry::with_provider` (a host's own provider
  under `name:<model>`) and `with_custom_provider`;
  `CustomProviderConfig::openai` and `anthropic`, and `Default` for
  `CustomProviderConfig` and `Dialect` (`OpenAi`); inherent `resolve`, `id`
  and `stream`. The facade's `testing` feature adds `FakeHttp`, `chat_sse`
  and `typed_sse`.
- haest-tools: `BuiltinTool`, a public `SkillTool`, `NewJob`, `JobOutput`,
  `JobState`. haest-workspace: `SandboxWorkspace::with_commands`.
  haest-mods: `ModOptions::with_mod`, `with_approval` and `with_plan`,
  `ModUi::warn`. haest-config: `Keys`, `Kind::Url`.
- haest-runtime: `RuntimeInputs::with_mod` (a host's own native mod) and
  `with_models` (the host's own `ModelResolver` in place of the built-in
  providers).
- The facade re-exports `CancellationToken` and `serde_json`, and the
  prelude adds `CancellationToken`, `Value`, `json!` and `BoxError`, so `use
  haest::prelude::*;` is the only import an `impl Tool`, `impl Provider` or
  `impl AgentHooks` needs.
- Every public type implements `Debug`, and none shows a key, a token, a
  header's value or a variable's value. Every `with_*` setter,
  `TurnOutcome`, `Hold`, haest-transport's `Attached` and haest-session's
  `Hold` are `#[must_use]`, so a dropped result warns.

### Fixed

Security:

- An approval shows the human the whole command (the mod cut it at 200
  characters), the `haest` command escapes control, bidi and zero-width
  characters in everything it prints from the model, a tool, a server or a
  peer, and it takes as the answer only what is typed after the question is
  on the screen.
- Debug output and errors show no credential: URLs lose their userinfo,
  query values and fragment, API keys and header and variable values show as
  `<redacted>`, and an MCP server's arguments as a count.
- A configured URL whose password holds an unescaped `/`, `?`, `#` or `@`,
  or that has no host (an empty variable), no longer sends its key to
  another host.
- A providers.json entry with a misspelt key or a `null` `baseURL` no longer
  sends the entry's key and the conversation to OpenAI's or Anthropic's own
  API.
- `GithubTracker` no longer reads, changes or deletes issues in other
  repositories with the host's token, follows a moved issue's redirect, or
  widens a search with `repo:`.
- A file or directory the sandbox moved or copied keeps its mode when
  applied (a 0600 key no longer lands 0644).
- On a disk that reads two names as one (macOS's default, exFAT), an apply
  no longer writes into the disk's other name or deletes the human's file
  under it: before each change it checks each name on the way with
  `read_dir` and `lstat`, and holds what it cannot place as a conflict. The
  sandbox reads the project as the system spells it, as bash does.
- An apply no longer deletes a file it moved where the move's write did not
  land (`mv src.md locked/` into a directory the disk will not write, or
  onto a conflict): it writes first, takes away last, and holds a delete or
  an overwrite as a conflict while a write that did not land carries its
  content.

Workspace and apply:

- The real workspace's timeout, interrupt or drop can no longer kill an
  unrelated process that reused bash's pid.
- An apply writes each file whole or not at all: a full disk or a kill
  leaves the human's version. Edit, Write and GenerateImage in the real
  workspace do the same (`Fs::write_whole`), and a write an NFS server
  refuses at close is reported.
- An apply never deletes a file the human saved after the sandbox edited it
  and removed its directory (it is a conflict), and an apply dropped partway
  keeps every unwritten change listed.
- After an apply, the sandbox sees the human's later edits, deletes and new
  files, and a change made while an apply runs waits for it.
- The sandbox keeps what it makes under a directory it removed and made
  again, and lists and applies the old files there.
- In the sandbox, `>>` and `tee -a` make a missing file, `mv a ./a` keeps
  the file, `mkdir -p notes/x` beside a file `notes` fails, and `mv a b`
  gives `b` the mode of `a`, as in bash.
- A mode the sandbox sets is listed and applied (execute bits only, to who
  may read the file), and `OverlayChanges::diff` shows a `Mode` line.
- GenerateImage writes over a file only at the path the model named;
  otherwise it picks a free `<stem>-<n>.<ext>`.
- `RealWorkspace` keeps at most 64 MiB of each output stream and stops
  reading a call's pipes once the call returns.
- `SandboxWorkspace` no longer panics on a `Duration::MAX` timeout, opens a
  relative directory as one absolute path, and ends a dropped call's task.
- `MemFs::rename` refuses what rename(2) refuses (a directory onto a file, a
  file onto a directory, onto a directory that is not empty) and then moves
  nothing. It overwrote and merged, so `mv` in a sandbox over a `MemFs`
  could lose files.

Sessions and turns:

- A resume after a torn last line or a failed append keeps the next record,
  and replay drops a tool result that answers no call.
- Two runs no longer write one session file (see Behaviour a host must act
  on).
- A turn after a dropped `Agent::run` answers the tool calls left without a
  result as interrupted, so the next request is accepted;
  `compaction::compact` does the same.
- The runtime waits for the session file before a tool with side effects
  starts (`before_effects`), so a crash cannot lose a call whose effect
  happened.
- A request over `max_request_tokens` no longer emits
  `LoopEvent::ModelStart`.

Providers and costs:

- The stream parser looks at each byte once, and a line or event past 16 MiB
  ends the message with `StopReason::Error`.
- A base URL with a query (a gateway key, Azure's `api-version`) gets the
  endpoint path before the query, for the providers and GenerateImage.
- `AnthropicProvider` keeps the thinking budget below `max_tokens` and reads
  `max_tokens: 0` as the default.
- A negative or non-finite cost is no price anywhere haest adds costs up
  (the ledger, `Usage`, the Code tool, GenerateImage, a server's
  `usage.cost`), so it can no longer lift or switch off a USD cap.
- Token counts and ids from outside stop at `u64::MAX` instead of panicking.
- `ProviderRegistry::resolve` asks the key source with nothing locked, so a
  slow key source no longer blocks other resolves.
- `GithubTracker` sends GraphQL to the right URL for an Enterprise root with
  a trailing CR, a space or upper case.

Concurrency:

- Ledger hooks, remedies and thresholds run with the ledger unlocked, so one
  that reads the ledger no longer hangs the turn, and a panic in host code
  no longer poisons the ledger or an agent's thresholds.
- The task board, the job hub and the subagent hub hand their state to hooks
  in change order, one call at a time.
- A dropped or panicking spawn gives back its `max_concurrent` place, and a
  stopped subagent counts until its turn has ended.
- A job never stays running after a host's `JobEvents` handler panics.

Other:

- A subagent stopped at its own cap gets a remedy naming the `subagent_*`
  setting.
- A huge `context_window` no longer overflows the default turn caps.
- A host that overrides a built-in tool by name no longer sends the model
  two tools of that name.
- `ModTools` over deferred tools lets the model call a deferred tool by
  name.
- An MCP tool's result is read through rmcp's own types: a resource link
  shows the model its URI, and structured content of `false`, `0` or `""`
  shows as JSON.
- haest-ts parses only `.tsx` files as TSX.
- The caps settings are listed under "Caps" again.
- `HttpError::Timeout` shows sub-second waits.
- The `haest` command: Ctrl+C stops a slash command (`-p` exits 130), a
  model it cannot use says what to do instead of always "Set its key", and
  `/budget tokens 0.5` is refused (it was cut to 0, which removed the cap).

### Removed

Each has its replacement under Breaking changes, or none:

- haest-core: `Usage::add`, `TurnOutcome::with_error`, `MemFs::with_files`,
  `ChildEnv::with`, `TaskBoard::with_clock`, `DiskSkills`,
  `Ledger::with_remedies`, `JsonlSessionFile::with_error_handler`,
  `SessionRecord::header`, `Fs::mkdir`, `impl ToolSource for Vec<Arc<dyn
  Tool>>`.
- haest-providers: `RegistryOptions`, the public `routing` module,
  `CustomProviderConfig::resolved_key`, `FakeReply::header`.
- haest-settings: `SubagentCapSettings`, `Settings::subagent_caps`.
- haest-tools: `agent_tools_with`, `SubagentHub::answers_due`,
  `builtin_word`, `skill_tool`, `SkillToolOptions::over`,
  `JobHub::with_events`; with no replacement, the Image API helpers
  `call_image_api`, `sniff_image_type`, `ImageCatalog`, `ImageError`,
  `ImageReply`, `ModelSettings` and `SettingSpec`.
- haest-workspace: `RealWorkspace::with_fs`,
  `SandboxWorkspace::overlay_changes`. haest-code: `NativeFn`,
  `NativeFuture`. haest-mods: `Mods::add`, `PlanListener`,
  `ApprovalGate::with_lines`. haest-mcp: `StdioTransport::with_env`.
- haest-runtime: `SessionError` (now `SessionFileError`), the assembly
  pieces listed under Breaking changes, `RuntimeHooks::ui`, `catalog` and
  `compact_now`.

## 0.2.0 (2026-11-17)

A breaking release that settles haest's names and seams before anyone else
builds on them. Every public item was reviewed: one word per concept, one
path per item, typed errors everywhere, and the HTTP client is a plug point.
Read "Upgrading" first; the per-crate sections list every change.

### Upgrading

1. Depend on `haest = "0.2"`.
2. `use haest::prelude::*;` in place of `use haest::*;`. The prelude holds
   the key types, every type their traits name, and `async_trait`.
3. Core's modules moved under `haest::core`: `haest::fs` is
   `haest::core::fs`, `haest::session` is `haest::core::session`, and so on.
   `haest::workspace` is always the haest-workspace crate.
4. "Cap" is the one word for a maximum that stops something: `limits` ->
   `caps`, `TurnLimits` -> `TurnCaps`, `LedgerCaps` -> `ConversationCaps`,
   and every cap type lives in `haest::core::caps`.
5. `model_spec` is the `provider:model` spec in every Rust name
   (`Settings::model_spec`, `Runtime::set_model_spec`, ...); `model` is a
   provider's own model id.
6. `AgentDeps` keeps an agent's own parts (`tools`, `system_prompt`); what
   the agents of one conversation share moved to `AgentDeps::shared`
   (`SharedDeps`): `deps.hooks` -> `deps.shared.hooks`.
7. Facts about a model and the clock left `AgentHooks`: `input_modalities`
   and `reasoning` -> `ModelFacts` (`SharedDeps::facts`), `now_ms` ->
   `Clock` (`SharedDeps::clock`), `limits` -> `TurnCapsSource`
   (`SharedDeps::turn_caps`).
8. A host's HTTP client implements `haest::http::HttpClient` (one `send`)
   in place of `haest_providers::Transport`.
9. No public function returns `Result<_, String>` any more. Show any haest
   error with `haest::core::error::report(&e)`, which prints its causes.

### Security

- The shared HTTP client (model calls, GenerateImage, the GitHub tracker)
  followed up to 10 redirects to any origin, and reqwest drops only
  `Authorization`, `Cookie` and `Proxy-Authorization` on the way:
  Anthropic's `x-api-key`, a providers.json entry's `headers` and, on a 307
  or 308, the whole prompt went to the redirect target. It now follows only
  a 307 or 308 to the first request's origin, at most 10; any other redirect
  comes back as the answer. A 301, 302 or 303 is no longer followed even
  within the origin (it turned a POST into a body-less GET).
- The net guard's IPv6 check was a blocklist with holes (site-local,
  IPv4-translated, Teredo, documentation and discard-only addresses all
  passed). IPv6 is now an allowlist: `2000::/3`, less `2001::/23`,
  `2001:db8::/32`, `2002::/16` and `3fff::/20`.
- The IPv4 blocklist missed the documentation ranges `192.0.2.0/24`,
  `198.51.100.0/24` and `203.0.113.0/24`. It now covers every range IANA
  marks not globally reachable, plus multicast.
- WebFetch's DNS-rebinding defence moved into its client: every name is
  resolved through the guard's own resolver when the client connects, a
  name with any blocked address is refused, and no redirect is followed by
  the client. What it connects to is what was checked.
- A host that configured its own reqwest client panicked for want of a TLS
  crypto provider. haest-http builds every client (`build_reqwest`,
  `ReqwestClient::configured`) and installs ring first when none is set.

### Bug fixes

- A resumed conversation's deferred tools stay loaded: a tool the model had
  loaded before a restart needed a new ToolSearch in `deferred_tools = grow`.
- The loop drops tool calls from a failed or aborted answer before it enters
  history; a call left there with no result failed the next request.
- `SubagentHub` gave every agent's Agent tool haest's default options and
  ignored `SpawnInput::subagent_type`. New `SubagentParent::agent_tool_options`
  and `SubagentParent::subagent_tools(subagent_type)` (both with defaults)
  carry a host's kinds of subagent.
- The sandbox mounted every skill dir read-only, the project's own included,
  so a project skill could not be created or edited. A skill dir inside the
  cwd is left to the overlay (editable, a change to review).
- `SandboxMounts` mounts nest as on a real mount table: a path belongs to
  the deepest mount holding it. A mount inside or around another was left
  out.
- `Fs::chmod` on the sandbox's files did nothing, so a mode set through a
  writable real mount never reached the disk.
- `js` in the sandbox shell dropped what was piped into it. The script has
  it as `process.stdin` and as `/dev/stdin`.
- `js` took `.tsx` files as TypeScript and failed with a bare syntax error
  on JSX. A JSX file (`.tsx`, `.mtsx`, `.ctsx`, `.jsx`) is refused up front.
- An ALWAYS approval could not be taken back once the gate became the
  host's `ModUi`. `Runtime::gate` and `ApprovalGate::revoke` do it.
- `split_frontmatter` closed the head on any line starting with `---`
  (`---foo` leaked into the body) and read an empty head as no frontmatter.
  A fence is now a line that is `---` plus trailing blanks, the opening one
  included, so `--- ` and a CRLF `---\r\n` open a head too.
- `slug` cut before trimming, so a cut on a dash left one at the end.
- `ensure_dir` returned Ok when a file sat at the path; the next write under
  it failed. It returns `NotADirectory`.
- `nearest_accepted_effort(Off, ..)` sent `low` to an API that lists none of
  haest's levels and refuses it; it sends no level.
- `media_type_from_name("png")` read the whole name as the extension, and
  `application/pdfx` counted as a PDF.
- `IssueFilter::default()` had `limit: 0`, so a list with the default filter
  was always empty.
- `ProviderRegistry::names()` listed a providers.json entry named like a
  built-in twice.
- An unknown `profile` listed `claude` twice when a config file added a
  `[profiles.claude]` table.
- `Resolved::source("env")` was always `Source::Default`; it is the last
  layer that passed a variable.
- An error's cause was printed twice ("... is not valid JSON: <serde>:
  <serde>") by `ProvidersFileError`, `EntryProblem` and `McpConfigError`.
  Their own text is their own words; `report` adds the causes once.
- GenerateImage built a new reqwest client (TLS setup, a fresh pool) for
  every request. An Image API answer past 64 MiB is refused.
- `GithubTracker::new` could panic building its client, and built one that
  `with_http` then threw away.
- The budget line read "0 unpriced tokens of no cap", and the 80% warning
  repeated the whole line. An uncapped measure reads "(no cap)", and the
  warning names only the cap it crossed.

### Behaviour changes

- A custom providers.json `"type": "anthropic"` entry is its own provider:
  its messages carry the entry's name, and signed thinking replays only to
  the provider that wrote it. A conversation saved before 0.2.0 with such an
  entry carries `anthropic`, so that entry drops its old thinking instead of
  sending it.
- A providers.json entry with a `type` other than `openai` or `anthropic`
  is refused (a typo was sent as Chat Completions), and so is an entry, or a
  file, that is not a JSON object. A `pricing` row is read through
  `ModelPricing`'s serde: a `cacheWrite` that is not a number is refused.
- haest-core, haest-tools and haest-workspace no longer name a CLI command
  or a `HAEST_*` variable in their text. The host adds its remedies
  (`haest::core::remedy`); the `haest` command's messages read as before.
- An MCP tool ending `__send_message` or `__list_conversations` defers like
  any other unless the host names it.
- With both instruction sources on, haest reads `~/.claude/CLAUDE.md` and
  `~/.agents/AGENTS.md` only.
- A model call's error for a dead connection or a broken body carries its
  causes behind a short lead, the same for every client.
- The wait for a model's status (ten minutes) holds for every client, a
  host's included, and a try that runs out is retried.
- `Runtime::run` flushes the session file when the turn ends.
- A stdio MCP server started by a bare command with no `PATH` handed in
  fails saying so, and how to hand one in.
- A guest calling a VM function the spec does not answer reads "no native
  function NAME".

### haest (the facade)

- Core's modules are under `haest::core` (`haest::fs` -> `haest::core::fs`);
  the key types stay at the root. `haest::workspace` is the haest-workspace
  crate; core's workspace module is `haest::core::workspace`.
- New `haest::prelude`. `haest_core::prelude` is the same list.
- New feature `http` (on with `providers` and `tools`) and `haest::http`.
- Feature `settings` no longer turns on `tools`.
- New `haest::runtime_inputs(loaded)` (features `config` + `runtime`): a
  loaded config as the runtime's inputs.
- `JsonMap` is at the root.
- The example `brain` is `notebook`: `cargo run -p haest --example notebook`.

### haest-core

Paths:

- `limits` -> `caps`: `TurnLimits` -> `TurnCaps`, `SubagentCaps` stays.
  `ledger::{LedgerCaps, LedgerCap, CapOverrides}` ->
  `caps::{ConversationCaps, ConversationCap, ConversationCapOverrides}`.
- Private modules: `tool_batch`, `tool_exec`, `guard`, `request_prefix`,
  `effort`, `pricing`, `panics`, `expand`. `effort::nearest_accepted_effort`
  and `pricing::book_priced_cost` -> `provider::{..}` (for a provider of a
  host's own). `tool_exec::MAX_TOOL_OUTPUT` -> `tool::MAX_TOOL_OUTPUT`.
- `__serde` -> `__private::serde`. `__private` is `doc(hidden)` and outside
  semver.
- `media::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS}` and
  `tool::FILE_TOOLS` -> haest-tools' root. `handed::{OPENROUTER_URL,
  openrouter_base_url}` -> hidden (a host sets the `OPENROUTER_BASE_URL` key).
- New modules: `error` (`report`, `BoxError`), `remedy` (`Limit`,
  `Remedies`), `frontmatter` (`split_frontmatter`, `block_end`,
  `inline_list`, `unquote`), `prelude`. New `tool::slug`.
- Now crate-private: `TurnGuard`, `tool_batches`, `run_tool_batches`,
  `RequestPrefix`, `build_request_prefix`, `error_assistant`, `fold_spend`,
  `tool_result_message`, `estimate_request_tokens`, `request_too_big`,
  `cut_index`, `media::{fit_media, fit_for_model, messages_for_estimate,
  user_message, media_kind_of}`, `Agent::request_prefix`, `Agent::sendable`.

The agent:

- `Agent`'s state is private: `messages` -> `history()` /
  `replace_history(..)`, `model_spec` -> `model_spec()` /
  `set_model_spec(..)`, `effort` -> `effort()` / `set_effort(..)`.
- `AgentOptions::model` -> `AgentOptions::model_spec`.
- `AgentDeps::{models, cwd, workspace, spill_dir, hooks, ledger,
  web_search}` -> `AgentDeps::shared: SharedDeps`, which also holds the new
  `facts`, `clock`, `turn_caps` and `remedies`. `AgentDeps::new` is
  unchanged; new `SharedDeps::new(models, cwd, workspace)`.
- `AgentHooks::limits(spec)` -> `TurnCapsSource::turn_caps` on
  `SharedDeps::turn_caps`: a `TurnCaps` is a source, and so is a closure
  `Fn(&str) -> Option<TurnCaps>`.
- `AgentHooks::{input_modalities, reasoning}` -> trait `ModelFacts` on
  `SharedDeps::facts` (default `NoFacts`).
- `AgentHooks::now_ms` -> trait `Clock` on `SharedDeps::clock` (default
  `WallClock`; any `Fn() -> u64` is a clock). `TaskBoard::with_clock` takes
  `impl Clock`.
- `AgentHooks::before_model_call` returns `Result<(), BoxError>`.
- `TurnEnd` (the struct `Agent::run` returns) -> `TurnOutcome`.
- `impl Default for AgentControl` is gone: take one from `Agent::control()`.
- `LoopEvent` derives `Deserialize`, so a Rust program reads `--json` lines
  back.

Caps and the ledger:

- `TurnCaps::max_turn_ms: u64` -> `max_turn_time: Duration`;
  `SubagentCaps::minutes: u64` -> `time: Duration`. `TurnCaps` no longer
  derives serde.
- `billed_tokens(u)` -> `u.context_tokens()`.
- `Ledger::usd` / `unpriced_tokens` (fields) -> methods.
- New `Ledger::with_remedies`: the stop and the warning end with the host's
  remedy.

Errors (each was a `String`):

- `ModelResolver::resolve` and `provider::parse_model_spec` return
  `ModelError`: `BadSpec`, `UnknownProvider`, `MissingKey`,
  `Unavailable { provider, source }`.
- `CompactionError::Model` holds a `ModelError`.
- `media::media_from_bytes` returns `MediaError` (`NotMedia`, `TooBig`).
- `task_board::TaskError` is an enum (`UnknownId`, `BlocksItself`);
  `session::InvalidSessionId` is `{ id }`.
- `word_enum!`'s `FromStr::Err` is `words::UnknownWord { what, word,
  expected }`; `WordSet::from_words` returns it; `word_set!` takes a label:
  `word_set! { pub Name ("label") { .. } }`.
- `StopReason`, `InputModality`, `OpenRouterEffort`, `SummaryPath`,
  `ChangeKind`, `ApplyFailureCode` and `SkillSource` are `word_enum!`s.
- New `testing::assert_causes_said_once(&e)`, for a test of a host's own
  errors.

Names:

- `tool::{ToolHost, NoHost}` -> `ToolServices`, `NoServices`;
  `ToolContext::host` -> `services`; `ToolServices::log` -> `debug`.
- `skills::SkillOpts` -> `SkillOptions`, `context::InstructionOpts` ->
  `InstructionOptions`; their `config_dir` -> `claude_dir` (no longer
  optional), as in `SkillScanRoots` and `read_installed_plugins`.
- `skills::skill_scan_roots(fs, &opts)` -> `SkillScanRoots::find(fs, &opts)`;
  `skills::scan_roots(&roots)` -> `roots.dirs()`; `SkillScanRoots` holds
  `{ options, plugins }`.
- `task_board::{HaestTask, HaestTaskStatus}` -> `Task`, `TaskStatus`.
- `compaction::CompactionConfig` -> `CompactionOptions`;
  `provider::WebSearchConfig` -> `WebSearchOptions`.
- `context::PromptInputs::model` -> `model_spec`; new
  `PromptInputs::tool_guide` (haest-tools writes it).
- `session::ReplayedSession::model` -> `model_spec`,
  `SessionRecord::Config { model }` -> `{ model_spec }` (the file's key is
  still `"model"`). `session::session_path` -> `session_file(&sessions_dir(..), id)`.
- `deferred_tools::DEFAULT_ALWAYS_LOADED` is gone (no MCP tool stays loaded
  unless the host names it). `DeferredTools::new` takes any `ToolSource`;
  `deferral_tools`, `ToolDeferral::advertise` and `callable` are private.
- `ChangeSet::apply` returns an `ApplyReport` (each change's failure is in
  it), not `io::Result`.

### haest-http (new)

The HTTP seam: `HttpClient` (one `send(Request)`), `Request`, `Response`,
`Body`, `Method`, `HttpError`, `BoxError`, and the default `ReqwestClient`,
built on its first request and shared by the process. `reqwest_builder()`
and `build_reqwest()` are the one place haest builds a reqwest client. It
re-exports `async_trait`, `Bytes` and `reqwest`; a breaking release of
reqwest is a breaking release of haest. WebFetch sends through a guarded
client of its own and an MCP server's streamable HTTP through rmcp's,
whatever client a host hands in.

### haest-providers

- `Transport` -> `http::HttpClient`: `post(req, cancel)` -> `send(req)`
  (cancelling is dropping the future). `{HttpRequest, HttpResponse,
  ByteStream, TransportError}` -> `http::{Request, Response, Body,
  HttpError}`. `ReqwestTransport` -> `http::ReqwestClient`;
  `with_client(c)` -> `ReqwestClient::configured(|builder| ..)`.
- `RegistryOptions.transport` -> `RegistryOptions.http`; the default builds
  no client until the first request. New `RegistryOptions::headers_timeout`
  and `with_headers_timeout` on each provider (default ten minutes).
- The providers take `Arc<dyn HttpClient>`. `testing::FakeTransport` ->
  `testing::FakeHttp`.
- `CompatOptions::new(id, flavor, api_key)` -> `CompatOptions::new(id,
  api_key)` with `flavor` a field (default `Generic`). New
  `AnthropicOptions::new(id, api_key)` and `ResponsesOptions::new(id,
  api_key)`; their `Default` is gone. New `AnthropicOptions::id`.
- `ProviderConfigError` is gone: a spec error is the `ModelError` itself, a
  missing key `ModelError::MissingKey`, a bad entry `ModelError::Unavailable`
  with an `EntryProblem` as its source. `ProviderRegistry::resolve_spec` ->
  `ModelResolver::resolve`.
- `load_custom_providers` returns `ProvidersFileError` (`Read`, `Parse`,
  `NotAnObject`). `CustomProviderConfig::from_json(name, v)` ->
  `from_json(v) -> Result<_, EntryProblem>`;
  `CustomProviderConfig::api_key(keys)` -> `resolved_key(keys)`.
- `RoutingEnv` -> `RoutingScopes` (also in `RuntimeInputs`, `Loaded`,
  `RegistryOptions`, `CompatOptions`). `routing::parse_routing` returns
  `serde_json::Error`. `resolve_provider_routing`, `ResolvedRouting`,
  `LayerScope`, `LayerLevel` and `is_empty_routing` are gone; the rest of
  the routing internals are crate-private. `RoutingPriority` and `Dialect`
  are `word_enum!`s.
- The modules `anthropic`, `compat`, `responses`, `catalog`, `registry` and
  `transport` are private; everything a host uses is at the root
  (`registry::Dialect` -> `Dialect`). The request-shaping functions are
  crate-private, and `CompatProvider::options()` is gone.

### haest-tools

- Every module is private; each item a host uses is at the root, once
  (`haest_tools::agents::AgentDirectory` -> `haest_tools::AgentDirectory`,
  `web_fetch::markdown_of` -> `markdown_of`, `net_guard::is_blocked` ->
  `is_blocked`).
- `agents::MAIN` -> `MAIN_AGENT`; `read::text_output` -> `read_text_output`.
  The `SubagentCaps` re-export is gone (`haest_core::caps::SubagentCaps`).
- `ImageTool` -> `GenerateImageTool`, `ImageToolOptions` ->
  `GenerateImageOptions`. `image::{ImageHttp, HttpReply, ReqwestImageHttp}`
  are gone: the options take an `Arc<dyn HttpClient>`. `call_image_api` and
  `ImageCatalog::model_params` take `&dyn HttpClient` and return
  `ImageError`. `image::settings` is crate-private except `ModelSettings`
  and `SettingSpec`.
- `WebFetchNet`'s fields are private: `WebFetchNet::new(resolve)` (and
  `Default`) is the guarded pair; `WebFetchNet::unguarded(resolve, http)`
  takes a client of the host's and gives up the connect-time check.
  `web_fetch::{HttpGet, HttpResponse, Body, ReqwestGet}` are gone.
- `assert_public_url` returns `Result<(), UrlRefused>`; `Resolve::resolve`
  returns `Result<_, BoxError>`.
- `SubagentHost` -> `SubagentParent`; its `deps()` returns `SharedDeps`.
  `AgentDirectory::spawn` and `deliver` return `AgentError` (`TooDeep`,
  `TooMany`, `UnknownAgent`, `Finished`, `Refused`).
  `AgentToolOptions::models` -> `model_description`;
  `SpawnInput::model` -> `model_spec`.
- `create_task_tools` -> `task_tools`, `create_skill_tool` -> `skill_tool`.
  `builtin_tools_where` returns `MissingOption`; `JobHub::start` returns
  `JobError`.
- New at the root: `DEFAULT_IMAGE_MODEL`, `DEFAULT_IMAGE_MODELS`,
  `FILE_TOOLS`, `tool_guide`, `touches_workspace`.
  New `AgentTurnGate::with_remedies`.
- Now private: `fs_walk`, `RunOutcome`, `BashOutputTool`, `KillShellTool`
  (made by `job_tools`), `exit_notice`, `host_port`.
- `http` re-exports haest-http.

### haest-workspace

- Unix only (macOS, Linux), said with a `compile_error!` on other targets.
- No bashkit type in the API: the `bashkit` re-export,
  `SandboxWorkspace::shell_fs()`, `BashkitFsOverHaest`, `HaestFsOverBashkit`,
  `OverlayChanges::new` and `OverlayChanges::recorder` are gone or
  crate-private.
- `SandboxMounts::writable(dir)` / `readable(dir)` -> `with_writable(dir)` /
  `with_readable(dir)`.
- `RealFs::new()` -> the unit value `RealFs`. `kill_group` is crate-private.
- New `SandboxWorkspace::with_remedies`.

### haest-code

- The modules `code`, `sandbox_js`, `vm` and `call_gate` are private; their
  items are at the root (`vm::VmSpec` -> `VmSpec`).
- `HostFn` -> `NativeFn`, `HostFuture` -> `NativeFuture` (its error is a
  `BoxError`), `VmSpec::host_fns` -> `native_fns`.
- `VmLimits` -> `VmCaps`, which implements `Default` (the Code tool's
  bounds); `run_in_vm` takes `caps`. `code::MEMORY_BYTES` is gone
  (`VM_MEMORY_BYTES`).
- `code_callable(t, extra)` / `code_tool_names(tools, extra)` ->
  `(t, allowed)` / `(tools, allowed)`: the host lists the file tools.
- `JsEnv` gains `stdin`. `js` strips TypeScript on the blocking pool.

### haest-mcp

- `McpTransport::{Stdio { .. }, Http { .. }}` ->
  `McpTransport::Stdio(StdioTransport)`, `McpTransport::Http(HttpTransport)`.
  `McpServerConfig::stdio(name, command)` / `http(name, url)` ->
  `McpServerConfig::new(name, McpTransport::stdio(command).with_args(..))`
  / `McpServerConfig::new(name, McpTransport::http(url).with_header(..))`.
- `McpFailure::error` is an `Arc<McpStartError>` (it was a `String`);
  `McpFailure` no longer derives `PartialEq`.
- Crate-private: `McpTool`, `mcp_kind`, `mcp_result_to_output`.
- The `rmcp` re-export stays; a breaking release of rmcp is a breaking
  release of haest.

### haest-mods

- `ModHost` -> `ModUi`: `notice` -> `info`, `log` -> `debug`.
  `ModEnv::host` -> `ui`, `ModEnv::notice` -> `info` (a mod file still calls
  `haest.notice`).
- `ModConfig` -> `ModOptions`. New `ModCaps` and `ModOptions::caps`
  (10 s slice, 128 MiB, 30 s load, as before) and `JsMod::load_with`.
- `JsMod::load` returns `ModLoadError`. `ModError` is gone: a `Mod` event
  returns `Result<_, BoxError>`.
- `ApprovalGate<P>` -> `ApprovalGate` (a clone is the same gate). New
  `ApprovalGate::with_lines(&self, f) -> impl ModUi` and `ModLine`.
  `ApprovalGate::prompter()` is gone.
- `Answer` -> `ApprovalAnswer`. `Decision` is a `word_enum!`.
- New `PLANNING_TOOLS`, `PlanMod::with_planning_tools`, `SHELL_TOOLS`,
  `ApprovalMod::with_shell_tools`, and both lists in `ModOptions`.
- `mod_files` is crate-private.

### haest-settings

- `Limits` -> `CapSettings` (`Settings::limits` -> `Settings::caps`).
  `Settings::subagents` is a `SubagentCapSettings` (minutes);
  `Settings::subagent_caps()` turns it into `SubagentCaps`.
- `Settings::limits_for` -> `turn_caps_for`, `ledger()` ->
  `conversation_caps()`, `web_search_config()` -> `web_search_options()`.
- `Settings::model` -> `model_spec`. `Settings::image_model` is an
  `Option<String>` (unset is the tool's default); `image_models()` and
  `code_tools()` are gone. The settings group "Limits" is "Caps".

### haest-config

- `Loader::load` returns `ConfigError`. `Loaded::env` -> `passed_env`.
- `get` / `set` -> `Config::get` / `Config::set` (`SetError`).

### haest-runtime

- The modules `inbox`, `mods`, `prompt`, `sessions`, `tools` and `world`
  are private; their items are at the root. `mods::load(hooks, fs,
  prompter)` -> `mods_for(hooks, fs, &gate)`.
- `Host` -> `RuntimeHooks`, `Agents` -> `RuntimeParent` (its `deps` is a
  `SharedDeps`). `Runtime::host` -> `hooks`, `Runtime::agents` ->
  `subagents`. New `Runtime::gate`.
- `Runtime::agent` is private: `agent()` reads it, `run(input, sink)` runs
  a turn, `compact(cancel)` compacts. New `human_turn`, `next_agent_turn`
  and `shut_down` replace the inbox and shutdown code every host wrote.
- `build` returns `BuildError` (`Session`, `Model`, `Workspace`); `clear`
  returns `SessionError`; `set_model` -> `set_model_spec`, returning
  `ModelError`; `compact` and `RuntimeHooks::compact_now` return
  `CompactionError`. haest-runtime no longer depends on anyhow.
- `BuildOptions::resume: Option<String>` -> `Resume { New, Latest, Id }`;
  `model_given` -> `model_spec_given`.
- `RuntimeInputs::env` -> `passed_env`; new `RuntimeInputs::http` (the
  model calls and GenerateImage send through it).
- `workspace_for` takes the remedies and returns `WorkspaceError`;
  `image_options` returns `NoImageKey`.
- `Ui::prompting` is gone; new `Ui::remedy`.
- `skill_opts` / `instruction_opts` -> `skill_options` /
  `instruction_options`; `AgentPrompt::model` -> `model_spec`.
- `MainTools::new(wrapped, hub, mode)` -> `MainTools::new(wrapped, hub)`.
- Crate-private: `InboxQueue::new`, `latest_in`, `new_session_id`,
  `platform`.

### haest-ts

- `strip_types` returns `StripError` (`TooLong`, `Syntax`, `Spawn`,
  `Panicked`).

## haest-issues 0.1.0 (2026-11-17)

First release, experimental, outside the facade: a `Tracker` trait, a
GitHub backend and a backend over markdown card files, with tools that let
a model list, search, create, update, link and delete issues. An example of
what a crate of tools for haest looks like.

## 0.1.0 (2026-11-16)

First release.
