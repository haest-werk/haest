# Changelog

Every crate in the workspace shares one version, except haest-issues, which
is experimental and versioned on its own. Wire formats are kept compatible
with the TypeScript haest unless an entry says otherwise: the session file,
`--json` NDJSON, `providers.json`, `.mcp.json`, the settings keys and the
`HAEST_*` variables.

## Unreleased

### Bug fixes

- The caps settings are listed under "Caps" again, as 0.2.0 meant; the
  `Limits` -> `CapSettings` type rename had renamed the heading to
  "CapSettings" in `docs/settings.md` and `haest settings`.

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
