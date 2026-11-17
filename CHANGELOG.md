# Changelog

Every crate in the workspace shares one version, except haest-issues, which
is experimental and versioned on its own. Wire formats are kept compatible
with the TypeScript haest unless an entry says otherwise: the session file,
`--json` NDJSON, `providers.json`, `.mcp.json`, the settings keys and the
`HAEST_*` variables.

## Unreleased

Other programs can watch and drive a conversation while it runs: a parent
process over stdio, a program on the same machine over a Unix socket, a
fleet view elsewhere through a broker. Three new crates carry it:
haest-wire (the format), haest-session (one stream, one driver) and
haest-transport (the connections). All of it is opt-in: each is a facade
feature outside `default`, every new setting is off by default, and the
`claude` profile leaves them off.

Thresholds: a meter crossing a mark fires an action. The guard's nudge, the
agent-turn gate's nudge, the ledger's warning and compaction are now
default thresholds with the same numbers and behaviour, a host adds its own
beside them, and wind-down (new, opt-in, off by default) is one of them.
Breaking for a host that writes `TurnCaps` or `Settings` out in full;
everything built with `::new`, `Default` or `..` compiles as before.

No `Arc`, `Box` or `dyn` in haest-core's API: every plug point takes the
value itself, and holds it in a handle named after its trait. The one
`dyn` left is a borrow for the length of a call (`&dyn Fs`, the net
guard's `&dyn Resolve`), which takes the value and its handle alike. Read
"Upgrading" first.

The workspace has a lint table (`[workspace.lints]` in the root
`Cargo.toml`), and every crate takes it: docs and `Debug` on every public
item, `pub` only where a host can reach it, no type a host gets but cannot
name, and unsafe code denied except where a site claims it with a reason.
Clippy's picks from its restriction and pedantic groups come with it: a
library never prints or exits, a public function documents how it fails
and when it panics, and every `with_*` setter is `#[must_use]`, so a host
that drops the value a setter returned (`deps.with_hooks(h);`, which never
set anything) now gets a warning. `env_var` and the `HashMap`
`KeySource` take a map with any hasher, and `&Thresholds` iterates in a
`for` loop.

### Upgrading

1. Plug points take the value itself, or an `Arc` of one the host already
   shares, and `AgentDeps::new` takes what the conversation runs on as one
   `SharedDeps`: `AgentDeps::new(Arc::new(models), Arc::new(tools),
   Arc::new("You are terse.".to_string()), cwd, Arc::new(workspace))` ->
   `AgentDeps::new(SharedDeps::new(models, cwd, workspace), tools, "You are terse.")`.
2. `AgentDeps` and `SharedDeps` keep their fields private: set a part with
   its `with_*` setter (`deps.shared.hooks = Arc::new(host)` ->
   `deps.with_hooks(host)`), read it with the getter of its name
   (`deps.shared.cwd` -> `deps.shared().cwd()`).
3. A tool list is `Tools`: `vec![Arc::new(Read) as Arc<dyn Tool>]` ->
   `Tools::new().with(Read)`, and `Vec<Arc<dyn Tool>>` -> `Tools` in every
   signature that took or returned one.
4. `ToolServices` is a slot of its own, no longer a supertrait of
   `AgentHooks`: delete `impl ToolServices for MyHooks {}`; hooks that do
   publish, report or log go in both slots,
   `.with_hooks(host.clone()).with_services(host)`.
5. `ToolContext` keeps its fields private: read a part with the getter of
   its name (`ctx.cwd` -> `ctx.cwd()`), build one with `ToolContext::new`
   and a `with_*` setter per part (`ToolContext { cwd, cancel, .. }` ->
   `ToolContext::new(cwd, workspace).with_cancel(cancel)`).
6. A `Workspace` hands out handles: `fn fs(&self) -> Arc<dyn Fs>` ->
   `fn fs(&self) -> FsRef` (`self.fs.clone().into()`). `Fs::mkdir` and
   `Fs::remove` lose their `recursive` flag: `fs.mkdir(dir, true)` ->
   `fs.create_dir_all(dir)`, `fs.remove(path, true)` -> `fs.remove_all(path)`.
   An `Fs` implements `remove_all` itself (`fs::remove_tree(self, path)`
   walks the tree).
7. A `Ledger` is shared by cloning it: `Arc<Mutex<Ledger>>` -> `Ledger`,
   `ledger.lock().unwrap().usd()` -> `ledger.usd()`,
   `.with_ledger(Arc::new(Mutex::new(ledger)))` -> `.with_ledger(ledger.clone())`.
8. A `match` on `LoopEvent`, `StopReason` or another haest-core event or
   error enum adds a `_` arm. `ProviderRequest`, `ExecOptions` and
   `ShellCall` are built with `new` and `with_*`:
   `ExecOptions { cwd, cancel, timeout }` ->
   `ExecOptions::new(cwd, timeout).with_cancel(cancel)`.
9. Smaller renames: `total.add(&u)` -> `total += &u` (`Usage`),
   `opt_bool(input, k)` -> `opt_bool(input, k)?`,
   `TaskBoard::with_clock(on_change, clock)` ->
   `let board = TaskBoard::new(on_change); board.set_clock(clock);`,
   `ledger.with_remedies(r)` -> `ledger.set_remedies(r)`,
   `board.create(task)` ->
   `board.create(task)?`, `ChildEnv::new([("K".into(), v)])` ->
   `ChildEnv::new([("K", v)])`, and `Arc<TaskBoard>` / `Arc<ToolDeferral>`
   -> `TaskBoard` / `ToolDeferral`.
10. A haest-runtime host hands in values and reads parts with getters:
    `build(inputs, Arc::new(ui), prompter, opts)` ->
    `build(inputs, ui, prompter, opts)`, `World::Fs(Arc::new(fs))` ->
    `World::files(fs)`, `inputs.keys = Arc::new(keys)` ->
    `RuntimeInputs::new(settings, world).with_keys(keys)`, and
    `rt.hooks.flush()` -> `rt.hooks().flush()`.
11. A flag setter takes a `bool`, and an optional field's setter takes the
    value itself: `SkillToolOptions::new(set).with_name_enum()` ->
    `.with_name_enum(true)`.
12. A `match` on a `ModLine` (the lines of `ApprovalGate::mod_ui`) adds
    `ModLine::Warn` and an arm for kinds to come:
    `ModLine::Info(t) => show(t)` -> `other => show(other.text())`. A
    `match` on `CommandOutcome` or `BashVerdict` adds a `_` arm too:
    `CommandOutcome::Handled => {}` -> `_ => {}`.

### Bug fixes

- A ledger hook that read the ledger it hears from (a warning shown with
  the budget's summary beside it) hung the turn for good: the hooks ran
  with the ledger locked. They run once it is unlocked; snapshots are still
  persisted one at a time, never an older one after a newer one.
- The caps settings are listed under "Caps" again, as 0.2.0 meant; the
  `Limits` -> `CapSettings` type rename had renamed the heading to
  "CapSettings" in `docs/settings.md` and `haest settings`.
- A subagent stopped at its own USD, step or time cap handed the host's
  remedy the turn cap's limit, so the `haest` command told the model to
  raise `max_turn_usd`, `max_turn_steps` or `max_turn_minutes` for a cap
  that `subagent_usd`, `subagent_steps` or `subagent_minutes` sets. The
  guard now takes the limit from the caps (`TurnCaps::subagent`).
- A huge `context_window` (any number up to `u64::MAX` is accepted)
  overflowed the default turn caps that follow it: a debug build panicked,
  a release build wrapped to a tiny cap. `max_turn_tokens` now stops at
  `u64::MAX`, and `max_request_tokens` is still exactly 90% of the window.
- `running_sessions` (and so `haest ctl list`) removed whatever path a
  discovery file's `socket` named whenever connecting to it failed for any
  reason: a crafted discovery file deleted an arbitrary file, and a passing
  error (too many open files) unlinked a live session's socket. It now
  removes a session's files only when the connection was refused or the
  socket is gone, and only when the socket is the `<stream id>.sock` beside
  its own discovery file; anything else is skipped and kept.
- `LineDecoder` searched its whole buffer for a newline again on every
  chunk, so a peer that sent a long line in small pieces, or never sent a
  newline, cost quadratic time: 16 MiB took about 3 seconds and the 64 MiB
  limit about 50. Each byte is now searched once.
- `AnthropicOptions`, `CompatOptions`, `ResponsesOptions` and
  `CustomProviderConfig` printed the API key and every header's value in
  their Debug output, and `Broker` and `Start` the broker token, so a host
  that logged one with `{:?}` logged the secret. Debug now shows
  `<redacted>` for the key and the token, and a header's name without its
  value.
- haest-http's `Request` printed its `Authorization` header and a URL's
  `user:password@` in its Debug output, and `Response` its `set-cookie`.
  Debug now shows each header's name without its value, the URL as
  `https://<redacted>@host/...`, and a request's body as its length.
- The Debug output of `AnthropicOptions`, `CompatOptions`,
  `ResponsesOptions` and `CustomProviderConfig` still printed a base URL's
  `user:password@` (a gateway that takes its credentials in the URL), and
  haest-issues' `GithubTracker` its API root's. They show `<redacted>@` in
  their place.
- haest-mcp's `HttpTransport` printed every header's value in its Debug
  output (an `Authorization` included), and `StdioTransport` every
  variable's in `env` (a server's token), and so did the `McpTransport` and
  `McpServerConfig` holding them. Debug now names each header and variable
  and shows `<redacted>` for its value.
- haest-config's `Loader` and `Loaded` printed the whole environment they
  were given in their Debug output, API keys included, and `Loaded` every
  providers.json entry's `apiKey`; `Config` printed each `env` variable's
  value and a `broker_url`'s `user:password@`. A host that logged one with
  `{:?}` logged the keys. Debug now names the variables, keys and entries
  and shows no value, and the broker URL as `<redacted>@host`.
- haest-issues' `GithubTracker` read, changed, deleted and linked issues in
  any repository the model named (`{"id": "victim/secret#1"}`), with the
  host's token, though it is one repository's tracker. The issue a call
  acts on must now be in its own repository, else the call is refused as
  `Invalid` and nothing is sent; only a link's other end may be elsewhere.
- The model providers' stream parser searched a line from its start again
  on every chunk, so a server that sent one long line in small pieces cost
  quadratic time on an executor thread (8 MiB in 1 KiB chunks: 16 s), and
  it buffered a line of any length. Each byte is now searched once, and a
  line or an event past 16 MiB ends the message with `StopReason::Error`,
  saying so.
- `ProviderRegistry::with_http(client).with_options(opts)` dropped the
  client: the options replaced everything, so the requests went over the
  shared reqwest client (a test's fake was bypassed and the network used).
  The registry now has one setter per option and no `with_options`.
- haest-ts parsed any file name ending in `x` as TSX (`box`, `index`), where
  a `<number>x` cast is a syntax error. Only a `.tsx` extension, in any
  case, is TSX now.
- `ProviderRegistry::resolve` asked the key source for a new provider's
  key with the registry locked, so a slow key source (a vault over the
  network) held up every other resolve, of providers already built too,
  and a key source that resolved a model itself locked the registry twice
  (a deadlock or a panic, by std's `Mutex`). The key is now
  asked with nothing locked.
- Token counts and ids that come from outside were added without a check:
  a session file's task id of 18446744073709551615 panicked the task board
  on restore, and token counts near `u64::MAX` (a provider's usage, a
  ledger snapshot, a durable run's `turn` record) panicked `Usage::add`,
  `Usage::context_tokens`, the ledger, the turn guard and the Chat
  Completions and Responses usage readers. Each count now stops at
  `u64::MAX`, and the task board refuses a new task once its ids have run
  out (`TaskError::IdsExhausted`) instead of handing an id out twice.
- `Agent::run` called while a turn was running (from a
  `before_model_call` hook, which holds the agent) was refused, but the
  refusal still re-armed the running turn's once-a-turn thresholds, so a
  `Rearm::NextTurn` reminder fired again at every step. The refused call
  now touches nothing, as its docs say.
- A warning threshold that fired in a subagent's turn never reached the
  human: the hooks a subagent runs with passed on the media and effort
  notices but not `AgentHooks::on_warning`. They pass it on now.
- haest-mods' `ModTools` over a source that keeps tools back (a
  `DeferredTools`) let the model call only the advertised ones: a deferred
  tool called by name was "Unknown tool". `ModTools::callable` now offers
  every tool the source can run, each inside the mods' hooks.
- A host that overrode a built-in tool the way the docs said, by adding its
  own under the same name (`builtin_tools(opts).with(MyRead)`), sent the
  model both: the request listed `Read` twice, which a provider may refuse.
  A `Tools` list now holds one tool per name: `with`, `push`, `extend`,
  `collect` and `From<Vec<ToolRef>>` put a tool in the place of the one of
  its name, so the list the model is sent and the tool that runs agree.
- A session could stall on an inbox arrival: one that reached the inbox
  after the driver had looked but before it started waiting was taken for
  "every sender is gone", so a background subagent's answer sat until the
  next request came (a quiet `--stdio` parent never sent one).
  `InboxQueue::wait` returns whether anything can still arrive, and returns
  at once when an arrival already waits (breaking, barely:
  `q.wait().await;` -> `if !q.wait().await { /* nothing will come */ }`).
- A person's input that reached a session while a turn ran was steered in
  as it was, skipping the mods' `prompt.submit` that every other message a
  person sends goes through. The session driver now runs a person's input
  through the conversation's rewriter (`Driven::rewriter`, the runtime's
  mods) once, steered in or not; a steer left unread runs next without a
  second rewrite. Another program's input is never rewritten, as before.
  The wire JSON is unchanged.
- One connection could make unbounded work: every request ran on a task of
  its own and the connection never stopped reading, so a peer with only
  watch access that sent `history` 100,000 times without reading made
  100,000 tasks, each holding a copy of the history. A connection now has
  at most 32 requests waiting for their replies; at the cap it reads no
  more lines until one is answered, so the rest wait in the peer's pipe.
  The wire JSON is unchanged.
- haest-mcp's `HttpTransport` (and the `McpTransport`, `McpServerConfig`
  and `RuntimeInputs` holding one) printed its URL in Debug as it was, so
  `https://user:s3cret@docs.example.com/mcp?api_key=k3y` showed the
  password and the key; `Broker` did the same with its URL, and
  `TransportError::Url` displayed it. A URL in haest's Debug output now
  shows no userinfo, no query value and no fragment
  (`https://<redacted>@docs.example.com/mcp?api_key=<redacted>`): this
  covers haest-http's `Request`, the providers' options and
  `GithubTracker` too, and `TransportError::Url` holds the URL shown that
  way. The `haest` command's "Broker link:" line kept the userinfo; it
  shows the URL the same way.
- `connect_session` (and so `haest ctl`) dialed whatever socket any
  discovery file in the dir named, so a crafted `<id>.json` sent the
  client to another program's socket. It now dials only the
  `<stream id>.sock` beside the discovery file, and `running_sessions`
  neither dials nor lists a file that names another path.
- `ReqwestClient`'s errors held the request's whole URL, as reqwest gives
  it, so a model call, a GitHub call or GenerateImage that failed to
  connect, or ran into a redirect loop, showed a key in the URL's query
  (`?key=...`) in its text and its Debug output. The URL in an
  `HttpError`'s cause now has no query, no fragment and no userinfo; the
  host and path still say where the request went.
- An MCP server reached over HTTP with its key in the URL
  (`https://mcp.example.com/mcp?api_key=...`) showed the key whenever a
  request to it failed: in `McpStartError`'s cause (so in `report`, the
  hub's log line and `McpHub`'s Debug output), in the warning haest-runtime
  gives for a server that did not start, which the `haest` command puts
  into the session stream (`--json`, `haest ctl`, the broker), and in a
  failed tool call's answer to the model. rmcp's HTTP client now runs on a
  reqwest client whose errors name the URL without its query, fragment or
  userinfo.
- `StdioTransport` printed its arguments in its Debug output, where
  `.mcp.json` puts a filled-in `${API_TOKEN}` (mcp-remote's `--header
  "Authorization: Bearer ${API_TOKEN}"`), so `McpServerConfig`,
  `ProjectMcp`, haest-config's `Loaded` and haest-runtime's
  `RuntimeInputs` showed the token. Debug now shows how many arguments
  there are.
- `AnthropicProvider` sent a Claude 4.5 or older model a thinking budget
  at or above the request's `max_tokens` (`with_max_tokens(4096)` with
  high effort sent a budget of 16,384), and sent `max_tokens: 0` as
  given: Anthropic refuses both with a 400, every call. The budget is now
  kept below the cap, a cap of 1,024 or less (no room for the smallest
  budget) sends no thinking, and a cap of 0 is the default 32,000, as the
  other providers already read it.

### Behaviour changes

- Every `haest --json` line is in the wire's envelope:
  `{"v":1,"sid":"<stream id>","seq":n,"ts":ms,"path":["main"],"type":...}`
  followed by the fields it always had, so a reader of the old lines keeps
  working. A `notice` and an `ask` line are numbered in the same stream as
  the events. The stream also carries `status` frames (`idle`, `running`),
  and a subagent's life comes as `subagent_started`, its loop events and
  `subagent_stopped` under `["main", "<id>"]`, in place of `notice` lines
  with its name.
- A flag the model sends as `"yes"` or `1` (`replace_all`,
  `run_in_background`, `allow_large`, Grep's switches, haest-issues'
  `remove`) is an input error the model reads, where it was a silent
  false. `"True"` and `"FALSE"` read as the flag they spell.
- The `haest` command runs its conversation as a session: the terminal is
  one subscriber of the stream, and what the human sees is as before.

### The haest command

- `--stdio` (`HAEST_STDIO`, never a config file): driven by the parent
  process, requests on stdin and frames on stdout, from the start of the
  stream, with no terminal and no REPL. The parent has command access and
  its turns are a person's. Approval asks are `approval_ask` frames it
  answers (allow, always, deny), withdrawn on an interrupt. haest exits 0
  when stdin closes (once what was asked has finished) or after a `close`,
  and 1 when reading stdin fails. A prompt, `-p` or `--json` with it is a
  usage error.
- `control_socket = "watch" | "command"` opens a control socket in
  `control_dir` (default `<data_dir>/control`), and haest says how to follow
  it. Input over it is another program's: the turn gate counts it, and at an
  open prompt it waits for Enter. A socket that cannot open is a warning.
- New `haest ctl` (Unix): `list`, `inspect <id>`, `events <id> [--after N]
  [--follow]` (shown as the terminal shows it, each input as `> ...`),
  `send <id> <text>` (waits for the turn it joins) and `interrupt <id>`;
  `--json` where it reads. An id may be a prefix only one session has.
- `broker_url` links the conversation to a broker over a WebSocket, its
  token the key `HAEST_BROKER_TOKEN` (never a setting; like a provider's key
  it stays with haest). `broker_access` is `watch` by default. haest says it
  linked; a broker that refuses the token is a warning and is not dialed
  again; a closing run gives the link up to 2 seconds to hand over the last
  frames.
- `control_socket`, `control_dir`, `broker_url` and `broker_access` come from
  your own file, a variable or a flag, never from a project's file or a
  profile: a repository cannot open your conversation to anyone.
- A watcher's terminal writes an answer whole when its streamed pieces were
  not seen (replayed from the ring), and shows approval asks and their
  withdrawal as lines.
- The remedy for a cap names its setting through `Limit::setting()`, and
  the three subagent caps have one.

### haest (the facade)

- New `reqwest` feature, in `default`: the default client over reqwest and
  rustls. A host that sends the model calls through its own client takes
  `default-features = false, features = ["providers"]` and builds no
  reqwest, rustls or ring (119 crates in the tree become 42); `tools` and
  `mcp` still turn it on (WebFetch and rmcp run on reqwest).

- `haest::CancellationToken` (tokio-util's, as `before_model_call`,
  `ToolContext::cancel` and `ProviderRequest::cancel` hand it out) and
  `haest::serde_json` are re-exported, and `haest::prelude` adds
  `CancellationToken`, `Value` and `json!`: `use haest::prelude::*;` is the
  only import an `impl Tool`, `impl Provider` (with `answer_stream`) or
  `impl AgentHooks` (with `BoxError`) needs, and a host adds `tokio-util` or
  `serde_json` to its own Cargo.toml only to use them itself.

- Every example's `main` (and its test) returns a `Result` and uses `?`
  where it used `unwrap`; the examples and `tests/plug_in.rs` take
  `async_trait`, `serde_json` and `CancellationToken` from haest, and name
  each item by one path: the root one (`haest::Tool`, `haest::BoxError`)
  for what the prelude holds, the module one (`haest::core::run::RunKey`)
  for the rest. The notebook's host takes its session sink as `impl
  Into<SessionSinkRef>` and its gateway provider as a `ProviderRef`.

- The README's "Embed it" opens with the front page's quickstart, and its
  pins (and the front page's) say `0.3`. A test fails when the README's
  quickstart and the compiled one in the docs differ.

- The front page and every example build their deps the new way:
  `ProviderRegistry::new(keys)`, `SharedDeps::new(models,
  std::env::current_dir()?, RealWorkspace::new(env))` and
  `AgentDeps::new(shared, builtin_tools(BuiltinOptions::default()), "You are
  a careful engineer.")`, in a `main` that returns a `Result`. The plug-point table lists `Tools`,
  `&'static str` and closures as prompts, and `ToolServices`.

- New example `durable_run` (`cargo run -p haest --example durable_run`,
  also under `cargo test`): the finished-run shortcut, and a kill and a
  resume on the memory store.

- The `notebook` example runs its helpers on haest's `SubagentHub` through
  `SubagentParent::start_member`: each helper on a node run of its own (its
  token, model spec and caps), closed by `MemberRun::end`. A new panel run
  puts a helper in the background: the person writes to it (`deliver` with
  an `AgentMessage`), the panel hears the same `AgentMessage` in
  `SubagentEvent::Message`, its answer reaches main through
  `Inbox::submit_answer`, the run waits on `SubagentHub::settled`, and every
  helper reads the notebook's own note.

- New features, none in `default`: `wire` (`haest::wire`), `session`
  (`haest::session`; with `runtime`, the runtime behind a session; with
  `mods`, approval asks answered over the session), `transport`
  (`haest::transport`), `transport-stdio`, `transport-unix` and
  `transport-broker` (pulls a WebSocket client on rustls).
- New examples, each run by `cargo test`: `wire`, `session`, `stdio`,
  `control_socket` and `broker`.
- New feature `rmcp` (not in `default`; turns on `mcp`): haest-mcp's own
  `rmcp` feature, so a host of the facade reaches `haest::mcp::rmcp` and
  `McpHub::connect_transport` without depending on haest-mcp itself.
- `runtime_inputs` builds the inputs with `RuntimeInputs`' setters; a host
  in another world changes it on the result:
  `inputs.world = World::Fs(fs)` -> `inputs.with_world(World::files(fs))`.
- New examples, both run by `cargo test`: `threshold` (a host's own
  thresholds: remind the model at 60% of the window, warn the human at the
  third model call) and `wind_down` (`wind_down_at` with a host's
  subagents).
- Feature `testing` also turns on the providers' fakes when `providers` is
  on: `haest::providers::testing::{FakeHttp, chat_sse, typed_sse}`.

### haest-core

Plug points without `Arc` (breaking):

- New handles, one per plug point, named after the trait: `ToolRef`,
  `ToolSourceRef`, `SystemPromptRef`, `ModelResolverRef`, `WorkspaceRef`,
  `AgentHooksRef`, `ToolServicesRef`, `ModelFactsRef`, `TurnCapsSourceRef`,
  `ClockRef`, `FsRef`, `ChangeSetRef`, `ProviderRef`, `KeySourceRef`,
  `ledger::LedgerHooksRef`, `threshold::ThresholdRef`, `run::RunStoreRef`,
  `session::SessionSinkRef`, `shell_command::ShellCommandRef` and
  `skills::SkillSetRef`. Each is a cheap `Clone`, derefs to the trait
  object, and is built `From` the value, an `Arc` of it or an
  `Arc<dyn Trait>`, so one host object can fill several slots;
  `XRef::new(value)` and `XRef::ptr_eq` besides.
- `AgentDeps::new(shared, tools, system_prompt)` takes what the
  conversation runs on as one `SharedDeps` (a subagent's deps from its
  parent's are the same call), and `SharedDeps::new(models, cwd,
  workspace)` takes each part as `impl Into<..Ref>` (`cwd` as
  `impl Into<PathBuf>`), so no two string slots sit side by side to trade
  places unseen (the prompt and the cwd both took a `&str`):
  `AgentDeps::new(Arc::new(m), Arc::new(t), Arc::new(p), cwd, Arc::new(w))`
  -> `AgentDeps::new(SharedDeps::new(m, cwd, w), t, p)`.
- `AgentDeps`'s fields are private: `deps.tools` -> `deps.tools()` and
  `with_tools(..)`, `deps.system_prompt` -> `system_prompt()` and
  `with_system_prompt(..)`, `deps.shared` -> `shared()` and
  `with_shared(..)`. Every `SharedDeps::with_*` is on `AgentDeps` too.
- `SharedDeps`'s fields are private, read by a getter of the same name and
  set by a consuming `with_*`: `shared.hooks = Arc::new(h)` ->
  `shared.with_hooks(h)`. The setters: `with_models`, `with_cwd`,
  `with_workspace`, `with_spill_dir`, `with_hooks`, `with_services`,
  `with_facts`, `with_turn_caps`, `with_clock`, `with_ledger`,
  `with_web_search`, `with_remedies`, `with_run_store`, `with_thresholds`.
- `AgentHooks: ToolServices` -> `AgentHooks: Send + Sync`. What tools hand
  the host is the new slot `SharedDeps::services` (`with_services`, default
  `NoServices`): `impl ToolServices for Hooks {}` -> nothing.
- New `Tools`: an agent's tool list. A cheap `Clone` (copy on write),
  `Tools::new().with(tool)`, `push`, `FromIterator`, `Extend`,
  `IntoIterator`; it derefs to `[ToolRef]` and is a `ToolSource`.
  `Vec<Arc<dyn Tool>>` -> `Tools` and `&[Arc<dyn Tool>]` -> `&[ToolRef]`
  everywhere: `ToolSource::tools`, `ToolSource::callable`,
  `deferred_tools::search_tools`, `deferred_tools::advertised_tools` (which
  takes `&Tools`). `impl ToolSource for Vec<Arc<dyn Tool>>` -> `Tools`.
- `SystemPrompt` is implemented for `&'static str` and for closures
  `Fn() -> String`, beside `String`.
- `ToolContext`'s fields are private, read by a getter of the same name
  (`ctx.cancel` -> `ctx.cancel()`, `ctx.tool_call_id` ->
  `ctx.tool_call_id()`, an `Option<&str>`) and set by a consuming `with_*`
  (`ctx.services = Arc::new(s)` -> `ctx.with_services(s)`).
  `ToolContext::new(cwd, impl Into<WorkspaceRef>)`; `workspace()` returns a
  `WorkspaceRef` and `services()` a `ToolServicesRef`.
- `Workspace::fs` returns `FsRef` (was `Arc<dyn Fs>`), `Workspace::changes`
  `Option<ChangeSetRef>` (was `Option<Arc<dyn ChangeSet>>`),
  `RunStore::sink` `SessionSinkRef` (was `Arc<dyn SessionSink>`);
  `ShellCall::fs` is an `FsRef`.
- `OutputChunks` is a struct built from a closure:
  `Arc::new(move |text: &str| ..)` -> `OutputChunks::from(move |text: &str| ..)`.
- `JsonlSessionFile::new` and `JsonlSessionFile::new_with_error_handler` take
  `fs: impl Into<FsRef>`. `DiskSkills::new(skills, fs)` ->
  `DiskSkills::new(fs, skills)`, the closure last, `fs` as
  `impl Into<FsRef>`.
- `Ledger` is a cheap `Clone` whose clones are the one ledger, and every
  method takes `&self` (`charge`, `set_cap`, `restore`, `reserve`,
  `release`): `Arc<Mutex<Ledger>>` -> `Ledger`. `SharedDeps::with_ledger`
  takes a `Ledger` and `SharedDeps::ledger` returns `Option<&Ledger>`.
  `Ledger::new(caps, impl Into<LedgerHooksRef>)`:
  `Ledger::new(caps, Arc::new(hooks))` -> `Ledger::new(caps, hooks)`.
  `LedgerHooks` are called after the ledger is unlocked and may read it.
  What changes every clone takes `&self` and says so:
  `Ledger::new(caps, hooks).with_remedies(r)` ->
  `let ledger = Ledger::new(caps, hooks); ledger.set_remedies(r);`.
- `ResolvedModel`'s fields are private: `ResolvedModel { provider:
  Arc::new(p), model: m.into() }` -> `ResolvedModel::new(p, m)`,
  `resolved.provider` -> `resolved.provider()` (a `ProviderRef`),
  `resolved.model` -> `resolved.model()` (a `&str`); `with_provider` and
  `with_model` besides.
- `testing::ScriptedProvider::new` returns the provider itself (was
  `Arc<ScriptedProvider>`): a cheap `Clone` whose clones answer from one
  script and record into one list, and `resolver(&self)`.
  `&Arc<ScriptedProvider>` -> `&ScriptedProvider`.
- New `skills::Skills`: a host's SKILL.md skills held where every clone sees
  them; `replace` after a rescan and the Skill tool and the prompt read the
  new list on their next call. A `SkillSet` over an `Fs`, like `DiskSkills`.
- `task_board::TaskBoard` is a cheap `Clone` whose clones are the one
  board: `Arc<TaskBoard>` -> `TaskBoard`, the host keeps one and the Task
  tools get another. Its clock is the board's too:
  `TaskBoard::with_clock(on_change, clock)` ->
  `let board = TaskBoard::new(on_change); board.set_clock(clock);`, the
  clock as `impl Into<ClockRef>`, and a clone made before reads it.
- `deferred_tools::ToolDeferral` is a cheap `Clone` whose clones share what
  is loaded: `Arc<ToolDeferral>` -> `ToolDeferral`.

`Fs` splits its flags (breaking):

- `Fs::mkdir(path, recursive)` -> `Fs::create_dir(path)` and
  `Fs::create_dir_all(path)`; `Fs::remove(path, recursive)` ->
  `Fs::remove(path)` and `Fs::remove_all(path)`. An `Fs` implements
  `create_dir`, `remove` and `remove_all`; `create_dir_all` has a default
  built on `create_dir`. `remove_all` has none, so a store's own rules for
  deleting a tree are never skipped by a default it did not know it got: a
  store with none writes `fs::remove_tree(self, path).await`, which walks
  the tree with `remove` and removes a link without walking into it.
  `MemFs`, `ShutFs` and every `Fs` in the workspace implement all four.

Room to grow (breaking):

- `#[non_exhaustive]` on `LoopEvent`, `ProviderEvent`, `ToolError`,
  `InnerCallEvent`, `ModelError`, `run::RunError`, `CompactionError`,
  `MediaError`, `TaskError`, `SessionRecord` and `StopReason` (providers
  keep adding stop words), and on the enums that grow with what models
  and hosts do: `AssistantPart` (a new block kind), `MediaKind`,
  `InputModality`, `Effort`, `WebSearchEngine`, `WorkspaceMode`,
  `threshold::Meter` and `threshold::Action`. A `match` outside haest-core
  adds `_ => ..`, and haest adds a variant in a minor release.
- `TurnOutcome` is `#[non_exhaustive]` (it gained `limit` once already):
  build one with `TurnOutcome::new` and `with_limit`; its fields stay
  readable.
- `AgentOptions` keeps its fields private: set them with `with_effort`
  (an `Effort` or an `Option` of one), `with_history` and
  `with_model_spec`, read them with the getter of the name:
  `let mut opts = AgentOptions::new(spec); opts.effort = effort;
  opts.history = saved.messages;` ->
  `AgentOptions::new(spec).with_effort(effort).with_history(saved.messages)`.
- `ProviderRequest`, `ExecOptions` and `ShellCall` are `#[non_exhaustive]`
  and built with `new` and a `with_*` per optional part; their fields stay
  readable. `ProviderRequest { model, system, messages, tools: vec![],
  cancel, effort: None, max_tokens: None, web_search: None }` ->
  `ProviderRequest::new(model, messages).with_system(system).with_cancel(cancel)`
  (also `with_tools`, `with_effort`, `with_max_tokens`, `with_web_search`).
  `ExecOptions { cwd, cancel: CancellationToken::new(), timeout }` ->
  `ExecOptions::new(cwd, timeout)` (`with_cancel`). `ShellCall { args, cwd,
  env, stdin, fs, cancel, ends_at }` -> `ShellCall::new(args, cwd, fs,
  ends_at)` (`with_env`, `with_stdin`, `with_cancel`).

Turn endings and small idioms (breaking):

- `TurnOutcome`, `run::RunEnd` and `run::RunOutcome` gain `limit:
  Option<Limit>`, the cap that stopped the turn (a turn cap, the
  conversation's budget, a request too big to send, a threshold that
  stops): `if let Some(Limit::Conversation(_)) = end.limit` instead of
  reading `error`. New `TurnOutcome::with_limit`. The `run_end` record does
  not keep it, so a stored result has None. A literal adds `limit: None`.
- `CompactionError::Failed(reason, usage)` ->
  `CompactionError::Failed { reason, usage }`.
- `error::BoxError` wherever the box was spelled out:
  `ModelError::Unavailable`'s `source`, and the examples' hooks.
- `Usage` adds up with `+=` (`&Usage` or `Usage`) and `Sum`:
  `total.add(&u)` -> `total += &u`, `calls.iter().sum::<Usage>()`.
  `Usage::add` is gone.
- `tool::opt_bool` returns `Result<bool, ToolError>`, like `opt_num` and
  `opt_str`: `if opt_bool(input, "all")` -> `if opt_bool(input, "all")?`.
  Missing, `null` and `""` are false; `true`/`false` in any case, as a
  boolean or a string, are the flag; anything else is `ToolError::Input`.
- New `CompactionOptions::new(context_window)`: compact at `COMPACT_SHARE`
  of the window, no ceiling, keep about the new `compaction::KEEP_RECENT`
  (12) messages, which is what haest-runtime does. `CompactionOptions {
  context_window, threshold: COMPACT_SHARE, ceiling: None, keep_recent: 12 }`
  -> `CompactionOptions::new(context_window)`.

Durable runs, opt-in: a host that hands no run store gets the haest it had.

- New module `run`: `RunKey` (`new`, `as_str`, `child`, `effect_key`),
  `EffectKey`, `input_hash`, `TurnState`, `RepeatCount`, `RunEnd`,
  `RunOutcome`, `Served` (`Fresh`, `Resumed`, `Stored`), `RunError`
  (`NoStore`, `InputMismatch`, `HistoryMismatch`, `TurnRunning`, `Store`),
  `ReplayedRun`, `replay_run`.
- New plug point `run::RunStore` (`sink(key)`, `load(key)`) and
  `run::MemoryRunStore` (`new`, `lines`, `keys`), set with
  `SharedDeps::with_run_store` (none by default). A subagent shares its
  parent's store. haest deletes no run.
- New `Agent::run_durable(&key, input, sink) -> Result<RunOutcome,
  RunError>`: a finished run under the key returns its stored result (no
  model call, no event), an unfinished one resumes, and another input
  under the key is refused. `Agent::run` never touches a store.
- `SessionRecord` gains `Run { t, key, input_hash, base }`,
  `Turn { t, state }` and `RunEnd { t, key, end }`: the kinds `run`, `turn`
  and `run_end` in the file. An exhaustive `match` adds them.
  `session::replay` skips them, and so does any reader that does not know
  them (the TypeScript haest included), so the conversation reads the same.
- `ToolContext` gains `run_key()`, `attempt()` (the new `tool::Attempt`:
  `First`, or `Resumed` when a resumed run runs the call again) and its
  shorthand `resumed()`, all set by the loop (`with_run_key`,
  `with_attempt`), and `effect_key()`. `ToolContext::new` has no run key
  and is the first attempt.
- New `Tool::on_resume() -> tool::OnResume` (`Rerun`, `Report`, `Lookup`;
  default `Rerun` for a read-only tool, else `Report`): what a call that
  may have run before the stop does on resume. A tool wrapper forwards it,
  as `DeferredTools` does.
- New `AgentHooks::before_effects()` (async, default returns at once),
  awaited before a batch of tool calls that holds a tool that is not
  read-only. A host that keeps its own session file flushes it there.
- The turn guard's repeat key is a hash that is the same in every build
  (FNV-1a), not `DefaultHasher`, whose output may change between Rust
  releases: a run resumed by a binary built with another toolchain still
  counts its repeats. Repeat detection within a turn is unchanged.

- New `Ledger::{reserve, release, available, held}` and
  `ledger::{Hold, NotAvailable}`: USD held on the conversation's budget.
  Available is the cap less what is spent and held, floored at 0, `None`
  with no USD cap. `Hold` is `#[must_use]`; `release` takes it by value.
  Holds live in memory: a snapshot does not save them, a restore keeps the
  ones in flight.
- `caps::SubagentCaps` gains `budgets: bool` (default false); a literal
  written out in full adds it.
- `ToolContext` gains `turn_time_left() -> Option<Duration>`: what is left
  of the turn's time cap on the host's clock, filled by the loop for every
  tool call (`with_turn_time_left`); `None` with no time cap, and in
  `ToolContext::new`.

Thresholds; wind-down, opt-in:

- New module `threshold`. A `Threshold` (a trait) is a `Meter` (context
  tokens, steps, turns, wall time, USD, tokens; each in a turn's or the
  conversation's `Scope`), a `Mark` (a share of the meter's cap, a value,
  or whichever comes first), an `Action` (remind the model, warn the human,
  wind down, compact, stop) and a `Rearm` (never, next turn, below the mark,
  every check). A `Site` checks a `Thresholds<S>` list and remembers what
  fired in a `Watch<S>`; a threshold a site cannot check is refused
  (`NotServed`). Sites: `TurnSite` and `LedgerSite` here, haest-tools'
  `AgentTurnSite`; compaction's threshold is `CompactionOptions`.
- A threshold goes in as itself: `Thresholds::try_with(impl Into<ThresholdRef>)`
  and `Thresholds::new(impl IntoIterator<Item = impl Into<ThresholdRef>>)`
  (`.try_with(SaveNotes)?`); `Site::defaults`, `WindDown::thresholds` and
  `Thresholds::iter` hand out `ThresholdRef`s. `Crossing`'s fields are
  private: `crossing.threshold()`, `crossing.reading()`.
- New `SharedDeps::with_thresholds(Thresholds<TurnSite>)`: what a turn
  checks, the guard's nudges unless set. Remind, warn and wind down at every step
  boundary (a resumed durable run's too); stop before every model call.
  Each agent keeps which fired for as long as it lives, and a durable run
  records the turn's in `TurnState::nudged`, so a resumed turn does not
  fire them again.
- New `AgentHooks::on_warning(text)`, default nothing: a turn's warning
  threshold fired, and the human must hear it. No new `LoopEvent`.
- New `Ledger::set_thresholds(Thresholds<LedgerSite>)`: the ledger's
  warning by default.
- `CompactionOptions` implements `Threshold`; `needs_compaction` is its
  check, unchanged. New `compaction::COMPACT_SHARE` (0.8).
- `__private::NUDGE_SHARE` -> `threshold::NUDGE_SHARE`; new
  `threshold::WARN_SHARE`.
- `TurnCaps` gains `context_window: u64` (the cap `Meter::ContextTokens`
  takes a share of; 0 = unknown) and `subagent: bool` (these are a
  subagent's caps: a stop's remedy and a wind-down refusal name the
  `subagent_*` setting).
- New module `wind_down`: `WindDown` (`WindDown::at(share)`, 0 = off, the
  default; `BadShare` outside 0 to 1), `Thresholds::<TurnSite>::wind_down(at)`
  adds its thresholds (the conversation's USD and unpriced tokens, the
  turn's USD, tokens and wall time, at one share). Past the mark a tool
  that starts new work refuses, and the model is told once a turn to
  conclude. `WindingDown` is what a refusal states, `WindDownWatch` what a
  tool asks.
- `ToolContext` gains `wind_down() -> &WindDownWatch` (never winding down
  by default; `with_wind_down`). A tool that starts new work asks
  `ctx.wind_down().check()`.
- `remedy::Limit` gains `SubagentSteps`, `SubagentTime` and `SubagentUsd`,
  and `Limit::setting()`, the plain name of the setting behind each limit.

Good citizens:

- Every public type has `Debug`: `Agent`, `AgentOptions` (`Clone` too),
  `AgentControl`, `AgentDeps`, `SharedDeps`, `ToolContext`, `Ledger`,
  `task_board::TaskBoard`, `deferred_tools::ToolDeferral`,
  `run::MemoryRunStore`, `session::JsonlSessionFile`,
  `shell_command::ShellCall` and `testing::ScriptedProvider`, so a host's
  `#[derive(Debug)] struct Host { deps: AgentDeps }` compiles. `ShellCall`
  shows its variables' names, never their values; `Agent` shows how long
  its history is, not the history.
- `TaskBoard::create` returns `Result<Task, TaskError>` and never hands an
  id out twice: once a task has the id `u64::MAX` (restored from a
  hand-edited session file, or the last one counted) it refuses with the
  new `TaskError::IdsExhausted`. `board.create(task)` ->
  `board.create(task)?`. The TaskCreate tool hands the refusal to the
  model.
- `ChildEnv::new` takes `(name, value)` pairs of anything that becomes a
  `String`, and a `ChildEnv` collects from them too:
  `ChildEnv::new([("PATH".to_string(), path)])` ->
  `ChildEnv::new([("PATH", path)])`. A pair written `("PATH".into(), ..)`
  no longer infers its type: drop the `.into()`.
- `ToolDeferral::with_always_loaded` and `deferring_all_except` take names
  of any `S: Into<String>`, as `load` does:
  `.deferring_all_except(["Read".to_string()])` ->
  `.deferring_all_except(["Read"])`.
- `JsonlSessionFile::new` and `new_with_error_handler` take `data_dir: impl
  AsRef<Path>`: `JsonlSessionFile::new(fs, Path::new("/data"), id)` ->
  `JsonlSessionFile::new(fs, "/data", id)`.
- New `answer_stream` (`provider::answer_stream`, in the prelude) and
  `provider::Deltas`: a provider's stream written as an async block that
  sends its deltas as they arrive (`deltas.text(..)`, `thinking`,
  `tool_call_start`) and returns the answer, which the stream ends with as
  its one `Done`. The `Provider` contract holds by construction, and the
  provider needs no stream crate:
  `Box::pin(stream::iter([.., ProviderEvent::Done(msg)]))` ->
  `answer_stream(|deltas| async move { ..; msg })`.
- New `tool::input(value) -> Result<JsonMap, ToolError>`: a tool's input
  from a JSON value, for a test or a host that runs a tool itself;
  `ToolError::Input` when it is not an object.
  `json!(..).as_object().unwrap().clone()` -> `tool::input(json!(..))?`.
- haest-core asks tokio for `rt` and `sync` only (a task and channels for
  the session file), so a plug-in crate that depends on haest-core alone no
  longer pulls in tokio's multi-thread runtime, macros and timers. The
  workspace's tokio entry names no features; each crate names its own.
- `BoxError` joins the prelude, so `haest_core::BoxError` (and
  `haest::BoxError`) names it: an `impl AgentHooks` returns it from
  `before_model_call`. `error::BoxError` still works.
- Every doc example's `main` returns a `Result` and uses `?`, inside its
  asserts too; `unwrap` and `expect` are left only on locks and where the
  trait gives no way out (a `SessionSink`'s `append`). The docs name an item
  the prelude holds by its root path (`haest_core::Agent`), everything else
  by its module path (`haest_core::run::RunKey`). `Ledger::reserve`
  documents its errors.

Hosts write less (round two of the review, and what porting brain onto
0.3 found):

- New `Tools::get(name)`, `contains(name)` and `remove(name)`, and
  `with_all(tools)` to chain a whole list on:
  `tools.iter().find(|t| t.name() == "Read")` -> `tools.get("Read")`,
  `let mut t = Tools::new().with(a); t.extend(more); t` ->
  `Tools::new().with(a).with_all(more)`. They take the place of the
  slice's `get` and `contains` (breaking): `tools.get(0)` -> `tools.first()`
  or `tools[0]`.
- A session file's header is built from a `SessionHeader` with named
  fields, so the session's id and the conversation's cannot trade places
  (breaking): `SessionRecord::header(sid, cid, cwd, ms)` ->
  `SessionRecord::from(SessionHeader { session_id, conversation_id, cwd,
  created_ms })`. `JsonlSessionFile::create` takes the cwd as a `&Path`:
  `file.create(cid, "/project", ms)` ->
  `file.create(cid, Path::new("/project"), ms)`.
- New `tool::ToolWrapper`: a tool made from another. It names the tool it
  wraps (`inner`) and implements only what it changes; every other `Tool`
  method forwards to the wrapped tool, in one place, so a wrapper that
  changes `execute` keeps the wrapped tool's `read_only`, `on_resume` and
  `canonical_input`, and a method `Tool` gains later. Every `ToolWrapper`
  is a `Tool`. The 8-method hand-written forwarder (which compiled with
  `on_resume` left out) -> `impl ToolWrapper for Noted { fn inner(&self)
  -> &ToolRef { &self.0 } async fn execute(..) { .. } }`. haest-mods'
  tool wrapper and the deferred tools' are built on it.
- New `ToolOverride` (in the prelude): a tool with its name, description
  or parameter schema changed and the rest the wrapped tool's.
  `ToolOverride::new(read).with_description("Read a notebook page.")`,
  pushed onto a `Tools` list, takes `Read`'s place.
- `OnResume` joins the prelude.
- New `ToolError::message()` and `ToolError::map_message(f)`: the same
  kind of error with its message changed, so a note added to every error
  needs no `match` and no `Err(e) => Err(e)` arm for kinds added later.
- New `SharedDeps::with_host(host)` (and on `AgentDeps`): one value in the
  hooks, services, facts, turn caps and clock slots at once, behind one
  `Arc` haest makes. `let h = Arc::new(host);
  deps.with_hooks(h.clone()).with_facts(h.clone()).with_turn_caps(h.clone()).with_clock(h)`
  -> `deps.with_host(host)`. `Clock::now_ms` (the wall clock) and
  `TurnCapsSource::turn_caps` (no caps) have default bodies now, so the
  slots a host has nothing to say in are an empty `impl`.
- The `SharedDeps` setters and their `AgentDeps` twins are made from one
  list, and every setter is `#[must_use]`. New `without_run_store()`.
- A setter of a slot that may be empty takes an `Option` of its value, so
  a host passes what it has without an `if let` (breaking where a `&str`
  went in): `SharedDeps::with_spill_dir` and `ToolContext::with_spill_dir`
  take `impl Into<Option<PathBuf>>`, `SharedDeps::with_ledger`
  `impl Into<Option<Ledger>>`, `ToolContext::with_run_key`
  `impl Into<Option<RunKey>>` and `ToolContext::with_turn_time_left`
  `impl Into<Option<Duration>>`. `.with_spill_dir("/data/spill")` ->
  `.with_spill_dir(PathBuf::from("/data/spill"))`, and
  `if let Some(d) = ctx.spill_dir() { inner = inner.with_spill_dir(d) }` ->
  `inner.with_spill_dir(ctx.spill_dir().map(Path::to_path_buf))`. The run
  store keeps taking the store itself (an `Option` of a handle would take
  only a `RunStoreRef`), and `without_run_store()` empties it.
- A `ProviderRef` is a `ModelResolver` (one provider serves every spec,
  the model being the part after the first colon), and so is a
  `ResolvedModel` (always that model):
  `move |_: &str| Ok::<_, ModelError>(ResolvedModel::new(p.clone(), m.clone()))`
  -> `ResolvedModel::new(p, m)`, as the models of `SharedDeps::new`.
- New `ResolvedModel::complete(req)` and `complete_streaming(req, on_delta)`:
  a model's whole answer outside the loop, the request sent to that
  model. `while let Some(ev) = stream.next().await { .. Done(m) => .. }`
  -> `resolved.complete(req).await`. It never fails: a stream that ends
  without its answer is an error answer, as the loop has always made one
  (whose provider is now the provider's id, was `none`).
- `LedgerHooks::persist` and `warn` have default bodies (nothing saved,
  nothing said), and new `ledger::NoLedgerHooks` is a ledger that reports
  nowhere: `struct Quiet; impl LedgerHooks for Quiet { fn persist(..) {}
  fn warn(..) {} }` -> `Ledger::new(caps, NoLedgerHooks)`.
- New `req_num(input, key)` (a required number) and
  `opt_count(input, key) -> Option<u64>` (a limit, an offset, a timeout:
  rounded down, held at `u64::MAX`, refused when negative), in the
  prelude: `opt_num(&input, "a")?.ok_or_else(|| missing("a"))?` ->
  `req_num(&input, "a")?`, and `(n.floor() as usize).max(1)` ->
  `opt_count(&input, "limit")?`. Read's `offset` and `limit`, Bash's
  `timeout` and Grep's `head_limit` and `context` read with it: a negative
  one is an input error the model reads (was: Read from line 1, Bash the
  shortest timeout, Grep a broken `-C`).
- New `ExecResult::ok(stdout)` and `ExecResult::failed(stderr, exit_code)`:
  `ExecResult { stdout, ..Default::default() }` -> `ExecResult::ok(stdout)`.
- `skills::InlineSkills` is `FromIterator<InlineSkill>` and
  `From<Vec<InlineSkill>>`, one list merged as `merged` merges it:
  `InlineSkills::merged([list])` -> `InlineSkills::from(list)`.
- `with_*` is a consuming setter that cannot fail, and nothing else
  (breaking): a constructor, a merge and a fallible add are named for what
  they do. `TurnOutcome::with_error(reason, why)` ->
  `TurnOutcome::error(reason, why)`, `MemFs::with_files(files)` ->
  `MemFs::from_files(files)`, `JsonlSessionFile::with_error_handler(..)` ->
  `JsonlSessionFile::new_with_error_handler(..)`, `base.with(&over)` (a
  `ChildEnv` merge) -> `base.overlaid(&over)`, and
  `Thresholds::default().with(t)?` -> `Thresholds::default().try_with(t)?`.
- `MemFs` is a cheap `Clone` whose clones are the one file system, so a
  host keeps one and hands a workspace another:
  `let fs = Arc::new(MemFs::new()); SandboxWorkspace::over(fs.clone())` ->
  `let fs = MemFs::new(); SandboxWorkspace::over(fs.clone())`. The notebook
  example and the plug-in test hold their own parts the same way (no
  `Arc::new` left in either host), and the notebook's host fills its slots
  with `with_host`.
- `Agent::run` and `Agent::run_durable` take `sink: &mut (impl EventSink +
  ?Sized)` (was `&mut dyn EventSink`): every caller compiles as before, a
  `&mut dyn EventSink` included.
- `TurnOutcome` is `#[must_use]`, so a turn that ended in an error is never
  dropped unseen: `agent.run(text, &mut sink).await;` warns ->
  `let end = agent.run(text, &mut sink).await;`. Every consuming setter
  (`ToolContext::with_*`, `ToolOutput::with_title`, haest-tools'
  `BuiltinOptions::with_*` and the rest) is `#[must_use]` too:
  `ctx.with_cwd(dir);` without its result warns instead of changing nothing.
- The do-nothing parts are all `Clone`, `Copy`, `PartialEq` and `Eq`:
  `ShutWorkspace` and `ShutFs` gain all four, `NoHooks`, `NoServices`,
  `NoFacts` and `WallClock` the last two.

### haest-http

- New `HttpClientRef`, the handle a part that sends holds its client in, in
  the shape of haest-core's handles: built from the client itself
  (`ReqwestClient::new().into()`), an `Arc` of one, or an
  `Arc<dyn HttpClient>`; cheap to clone, and it derefs to the client.
- `Method` and `HttpError` are `#[non_exhaustive]` (breaking): a `match`
  adds a `_` arm, and haest adds a method (`PUT`) or a variant in a minor
  release.
- `Method` implements `Display` and `FromStr` (the word on the wire,
  case-sensitive; a word it does not name is a `ParseMethodError`), and
  `Hash`.
- New `reqwest` feature, on by default: `ReqwestClient`, `build_reqwest`,
  `reqwest_builder` and the `reqwest` re-export (breaking for
  `default-features = false`, which now builds no reqwest, rustls or ring:
  105 crates in the tree become 25). `HttpClientRef` implements `Default`,
  the client every part uses unless handed one: the shared `ReqwestClient`,
  or without the feature a client that refuses every request
  (`HttpError::Request`, "no HTTP client: ... hand one in").
  haest-providers and haest-issues forward the feature as their own
  `reqwest`, the facade as `haest/reqwest`. In the workspace's
  `[workspace.dependencies]`, haest-http and haest-providers have default
  features off; haest-tools and haest-mcp ask for `reqwest` themselves.
- `ReqwestClient::send` is inherent: `client.send(req)` needs no `use
  haest_http::HttpClient` (an `HttpClientRef` never did).
- New `http` feature, off by default: conversions to and from the `http`
  crate for a host whose stack speaks it (an axum router behind a gateway,
  a hyper client). `http::Request<Bytes>: TryFrom<Request>` (an
  `HttpError::Request` for a URL or header the wire cannot carry),
  `Response: From<http::Response<B>>` for any `http_body::Body` (data
  frames streamed, trailers skipped) and `http::Method: From<Method>`.
  The 25 lines a gateway host wrote for it become `req.try_into()?` and
  `Response::from(res)`. With `reqwest` on it adds no crate to the tree.
- `Body` implements `Default` (empty) and `Request` implements `Eq`.

### haest-providers

- New `reqwest` feature, on by default: the default client (haest-http's
  `HttpClientRef::default`). With `default-features = false` no reqwest,
  rustls or ring is built (118 crates in the tree become 41), and a
  provider or registry not handed a client ends each message as an error
  saying to hand one in.
- `ProviderRegistry::new` takes only the keys, as `impl Into<KeySourceRef>`
  (an `Arc<dyn KeySource>` the host already shares still goes in as it is);
  providers.json's entries and every option come by setter on the registry,
  and `RegistryOptions` is gone (breaking):
  `ProviderRegistry::new(Arc::new(keys), custom, RegistryOptions { http:
  Arc::new(client), routing, ..Default::default() })` ->
  `ProviderRegistry::new(keys).with_custom(custom).with_http(client).with_routing(routing)`
  (also `with_retry`, `with_headers_timeout`, `with_app_title`,
  `without_app_title`, `with_app_url`), each read back with the getter of
  its name. `with_routing` takes the scopes or an `Option` of them.
- `ProviderRegistry::resolve` is inherent: `registry.resolve(spec)` needs no
  `use haest_core::ModelResolver`. The registry is still a `ModelResolver`.
  It is `Clone` and `Debug`: clones share what was built, so
  `Arc::new(registry)` -> `registry.clone()`; a `with_*` setter starts a
  cache of its own. Getters `keys()`, `custom()`.
- `AnthropicProvider::new`, `CompatProvider::new` and
  `ResponsesProvider::new` take `http: impl Into<HttpClientRef>` (breaking):
  `CompatProvider::new(opts, Arc::new(ReqwestClient::new()))` ->
  `CompatProvider::new(opts, ReqwestClient::new())`; an `Arc` of the host's
  client still goes in as it is. The providers are `Clone` (a clone shares
  the client) and `Debug`, with an `options()` getter.
- `AnthropicOptions`, `CompatOptions` and `ResponsesOptions` gain `with_*`
  setters beside their public fields: `with_base_url`, `with_header`, and on
  `CompatOptions` `with_flavor`, `with_reasoning_effort`, `with_pricing`,
  `with_routing`. `let mut opts = CompatOptions::new(id, key); opts.flavor =
  CompatFlavor::OpenRouter; opts.base_url = Some(url.into());` ->
  `CompatOptions::new(id, key).with_flavor(CompatFlavor::OpenRouter).with_base_url(url)`.
- `testing::FakeHttp::new` returns a `FakeHttp`, not an `Arc<FakeHttp>`
  (breaking): it is `Clone`, the clones sharing the script and the record, so
  a test hands `http.clone()` to a provider and reads `http.requests()`.
- `#[non_exhaustive]` on `CompatFlavor`, `Dialect`, `RoutingPriority`,
  `EntryProblem` and `ProvidersFileError` (breaking): a `match` adds a `_`
  arm.
- `ProvidersFileError::Read` says what failed: `cannot read
  /x/providers.json` (was the bare path).
- A host adds its own provider: `ProviderRegistry::with_provider(name,
  provider)` takes any `impl Provider` (or a `ProviderRef`) and resolves
  `name:<model>` to it, over a providers.json entry or a built-in of that
  name; `providers()` lists them. Before, it wrapped the registry in a
  `ModelResolver` of its own.
- A providers.json entry is written in Rust: `CustomProviderConfig` has
  `Default`, `openai(url)`, `anthropic(url)` and serde `Deserialize` (as
  `from_json` reads it), and `ProviderRegistry::with_custom_provider(name,
  config)` adds one. Before, the registry took only JSON.
- `CustomProviders` is a type of its own, not a `BTreeMap<String,
  serde_json::Value>` (breaking). `serde_json::from_str` still reads a
  providers.json text into one, each entry still checked when first
  resolved. `CustomProviders::from([(name, json)])` ->
  `serde_json::from_value(json!({name: json}))?` or
  `CustomProviders::new().with_entry(name, config)`; `custom.contains_key(n)`
  -> `custom.contains(n)`; `custom.keys()` -> `custom.names()`;
  `CustomProviderConfig::from_json(&custom[n])` -> `custom.get(n)` (the
  entry, checked). Its Debug output names the entries only.
- `CustomProviderConfig::resolved_key(&keys) -> String` ->
  `CustomProviderConfig::key(&keys) -> Option<String>` (breaking): None
  where it returned `"none"`, the placeholder the provider still sends.
- `AnthropicProvider`, `CompatProvider` and `ResponsesProvider` have
  inherent `id()` and `stream(req)`: `provider.stream(req)` needs no `use
  haest_core::Provider`. Each gains the getters `retry()`,
  `headers_timeout()` and `http()` beside its setters.
- `RetryPolicy` is `Copy`, `Eq` and `Hash`, and
  `ProviderRegistry::retry()` returns it by value (breaking for a caller
  that read it through a reference): `registry.retry().clone()` ->
  `registry.retry()`.
- `testing::FakeReply::header(&str, &str)` ->
  `FakeReply::with_header(name, value)`, taking `impl Into<String>` as
  `Response::with_header` does (breaking for tests).
- `testing::FakeReply` and `testing::FakeEnd` are `#[non_exhaustive]`
  (breaking for a test that matches on one): a `match` adds a `_` arm;
  building one is unchanged.

### haest-tools

- Tool lists are `haest_core::Tools` and single tools `ToolRef`:
  `builtin_tools`, `builtin_tools_where`, `agent_tools`, `job_tools`,
  `task_tools`, `SubagentHub::tools_for` and
  `SubagentParent::{base_tools, subagent_tools}` return `Tools` (was
  `Vec<Arc<dyn Tool>>`), `SubagentParent::tool_source` takes them, and
  `SubagentParent::system_prompt` takes `&[ToolRef]`.
- A subagent's lent services are their own wrapper (`ChildServices`), beside
  its lent hooks, now that `ToolServices` is a slot of its own.
- New `SubagentParent::start_member(&MemberStart, &CancellationToken)`,
  default `Member::in_process()` (the member haest has always run). It runs
  before a member's loop exists: an `Err` refuses the spawn (the name is
  freed, nothing runs, no end follows), an `Ok` hands back the member, a
  `Member` over the host's run (`Member::new(run)`).
- New `MemberRun`, every method a default: `model_spec()`, `caps()`,
  `deps() -> Option<SharedDeps>` (used as given, hooks included) and
  `end(MemberEnd)`, awaited once after the member's turn and before its
  answer goes back. New `Member`, `MemberStart`, `MemberEnd`,
  `MemberCaps`, `Ending` and `Spend`; `MemberStart`, `MemberEnd` and
  `Ending` are `#[non_exhaustive]`.
- `subagent_note` takes the `&MemberStart` it writes the note for (was the
  name, the parent's name, the description and `&SubagentCaps`, three
  strings in a row): `subagent_note(name, parent, description, caps)` ->
  `subagent_note(member)`.
- A background spawn's reply names the member's model spec.
- StopAgent on a member whose start has not returned: it never sends a
  model call, and its end is `Ending::Stopped`.
- Subagent budgets, opt-in (`subagent_budgets`): the Agent tool takes a
  `budget {usd, minutes}` when `AgentToolOptions::budget_caps` is `Some`,
  which `SubagentHub::tools_for` sets from its caps when
  `SubagentCaps::budgets` is on. Each value only tightens the caps; a time
  that does not fit what is left of the caller's turn is refused with what
  is left. The hub holds the budget's USD on the ledger while the member
  runs, so members running at once cannot overshoot the cap together.
- `SpawnInput` gains `budget` and `AgentToolOptions` gains `budget_caps`,
  both `Option<SubagentBudget>`. New `SubagentBudget { usd, time }`.
- `SpawnInput`, `SpawnOutcome` and `JobStatus` are `#[non_exhaustive]`
  (`SpawnInput` has grown twice): a host builds a spawn with
  `SpawnInput::new(prompt)` and `with_description`, `with_name`,
  `with_model_spec`, `with_subagent_type`, `with_budget` and
  `with_background(bool)`, and an answer with
  `SpawnOutcome::new(text, cost_usd)`; the fields stay readable. The
  optional fields' setters take the value itself, so
  `.with_subagent_type("explore")` compiles; a host holding an `Option`
  sets what it has. `SpawnInput { description: "tidy".into(),
  prompt: "Tidy up.".into(), ..Default::default() }` ->
  `SpawnInput::new("Tidy up.").with_description("tidy")`, and
  `SpawnOutcome { text, cost_usd }` -> `SpawnOutcome::new(text, cost_usd)`.
- `AgentError` gains `OverBudget { asked, available }` and
  `OverTime { asked, left, setting }`, and no longer derives `Eq`.
- `AgentDirectory::deliver` and `SubagentHub::deliver` take an
  `AgentMessage { from, to, text }` (was three strings in a row, the
  sender's name first): `hub.deliver("lead", "worker", "go")` ->
  `hub.deliver(AgentMessage { from: Sender::Agent("lead".into()), to:
  "worker".into(), text: "go".into() })`.
- New `Sender::{Agent, Person}` (`#[non_exhaustive]`): the person writes to
  a running subagent, or to main, under "[Message from <name>, the person
  you work for]". SendMessage is `Sender::Agent` and reads as before.
- `SubagentEvent` gains `Message(AgentMessage)`, the message as delivered,
  through `on_subagent` for every delivery (a refused one fires nothing).
  `SubagentEvent` is `#[non_exhaustive]`: a `match` outside haest-tools
  needs a `_` arm.
- New `Inbox::submit_answer(text)`, default `submit`: the hub hands a
  background answer for main to it, and a message for main to `submit`
  (was: both to `submit`).
- New `SubagentHub::idle()` and `SubagentHub::settled(timeout) -> bool`:
  no spawn in flight, each counted from the call until its answer is back.
  `settled` is true once idle, false when the timeout runs out.
- New `SubagentParent::subagent_note(&MemberStart) -> String`, default
  haest's `subagent_note`: the note under a member's system prompt, given
  the model spec and caps its run names.
- New `AgentTurnSite` and `AgentTurnGate::with_thresholds`: the gate's nudge
  is its default threshold.
- The Agent tool starts nothing while its turn winds down, whatever
  `AgentDirectory` it speaks to, before it looks at a budget: new
  `AgentError::WindingDown(WindingDown)`, which says what was spent of which
  cap, then the host's remedy.
- The hub runs its subagents on caps marked `subagent`.
- `JobHub` is a cheap handle: clones share the jobs, and only dropping
  the LAST clone kills the jobs still running (dropping one clone used to
  kill them all). Hold it, not an `Arc<JobHub>`; `job_tools` takes it by
  value.
- `JobHub::new` takes the inbox itself (`impl Into<InboxRef>`), and
  `set_events(&self)` sets the listener of every clone:
  `JobHub::with_events(Arc::new(tx), Arc::new(events))` ->
  `let hub = JobHub::new(tx); hub.set_events(events);`. New `InboxRef` and
  `JobEventsRef`.
- `JobHub::start` takes a `NewJob` (the command, and a description the
  job list shows, the command when none is set) and the `&ToolContext` it
  runs in:
  `jobs.start(ctx.workspace().clone(), cmd, desc, ctx.cwd().into())` ->
  `jobs.start(NewJob::new(cmd).with_description(desc), &ctx)`.
- `JobEvents::on_output(job, chunk, done: bool)` ->
  `on_output(job, JobOutput::Chunk(chunk) | JobOutput::Done)`.
- `Job::state()` returns a `JobState` that implements `Display` (was
  `String`): `format!("{}", job.state())` reads as before.
- `BashOptions`' fields are private: `BashOptions { jobs: Some(hub), .. }`
  -> `BashOptions::default().with_jobs(hub)`, and `with_default_timeout`;
  read with `jobs()` and `default_timeout()`.
- `Job`, `Taken` and `JobError` are `#[non_exhaustive]`; `BashTool`
  implements `Debug` and `Clone`.
- `SubagentHub` is a cheap handle: clones share the subagents, and
  `SubagentHub::new` takes the parent itself (`impl Into<SubagentParentRef>`)
  and returns the hub: `SubagentHub::new(Arc::new(parent), caps)` (an
  `Arc<SubagentHub>`) -> `SubagentHub::new(parent, caps)`. Its `spawn`,
  `deliver` and `stop` are inherent: no `AgentDirectory` import to call them.
- One `agent_tools(hub, me, opts)`, over `impl Into<AgentDirectoryRef>`;
  whether the caller may still spawn is `AgentToolOptions::can_spawn`
  (default true): `agent_tools_with(Arc::new(dir), me, false, opts)` ->
  `agent_tools(dir, me, AgentToolOptions { can_spawn: false, ..opts })`,
  and `agent_tools(dir, me, true)` -> `agent_tools(dir, me,
  AgentToolOptions::default())`. New `AgentDirectoryRef` and
  `SubagentParentRef`.
- `AgentDirectory::spawn` takes `&CancellationToken` (was by value).
- `SubagentParent::start_member` returns `Result<Member, AgentError>`
  (was `Box<dyn MemberRun>`): `Ok(Box::new(run))` -> `Ok(Member::new(run))`.
  `SubagentParent::tool_source` returns a `ToolSourceRef`:
  `Arc::new(source)` -> `source.into()`.
- `SubagentTypes`' fields are private, and it is built from its default
  kind, which is always one of the kinds, so building it cannot fail:
  `SubagentTypes { names, description, default }` ->
  `SubagentTypes::new(default).with_kinds(names).with_description(description)`
  (the default goes first when `names` leaves it out). Read with `names()`,
  `default_kind()` and `description()`.
- `AgentError` is `#[non_exhaustive]`; the agent tools implement `Debug`.
- `BuiltinOptions`' fields are private; it is built with setters:
  `BuiltinOptions { tasks: Some(Arc::new(board)), web_fetch: None,
  ..Default::default() }` ->
  `BuiltinOptions::new().with_tasks(board).without_web_fetch()`, and
  `with_read_intercept`, `with_bash`, `with_web_fetch`, `with_skill`.
- New `BuiltinTool` (`#[non_exhaustive]`), a built-in by its word: it
  reads and parses as the word (`Display`, `FromStr` with
  `ParseBuiltinToolError`), `BuiltinTool::ALL` lists them, and
  `BuiltinTool::of(tool_name)` replaces `builtin_word`:
  `builtin_word(name) == Some("tasks")` ->
  `BuiltinTool::of(name) == Some(BuiltinTool::Tasks)`.
  `builtin_tools_where` picks by it: `|w| set.has(w)` ->
  `|t| set.has(t.as_str())`. `MissingOption` names the tool (`tool:
  BuiltinTool`, was `word: &'static str`) and is `#[non_exhaustive]`.
- `ReadTool::new()` takes nothing and the intercept is a setter:
  `ReadTool::new(Some(Arc::new(i)))` -> `ReadTool::new().with_intercept(i)`.
  New `ReadInterceptRef`.
- `WriteTool`, `EditTool`, `GrepTool` and `GlobTool` are
  `#[non_exhaustive]` with `new()`, `Default`, `Debug` and `Clone`:
  `ToolRef::new(WriteTool)` -> `ToolRef::new(WriteTool::new())`.
- `SkillTool` is public and `skill_tool` is gone: `skill_tool(opts)` ->
  `SkillTool::new(opts)`. `SkillToolOptions::new` takes any `SkillSet`
  and `over` is gone: `SkillToolOptions::new(move || list(), Arc::new(fs))`
  -> `SkillToolOptions::new(DiskSkills::new(fs, move || list()))` (or
  core's `Skills`), `SkillToolOptions::over(Arc::new(set))` ->
  `SkillToolOptions::new(set)`.
- `SkillToolOptions::with_name_enum` takes the flag, as every flag setter
  does (breaking): `.with_name_enum()` -> `.with_name_enum(true)`.
- `task_tools` takes the board itself, a handle:
  `task_tools(Arc::new(board))` -> `task_tools(board)`.
- `DeferredTools::new` takes `impl Into<ToolSourceRef>`, `deferral()`
  returns `&ToolDeferral` (was `&Arc<ToolDeferral>`), and it is `Clone`
  (clones share the deferral) and `Debug`.
- `tool_guide` and `touches_workspace` take `&[impl AsRef<str>]`:
  `tool_guide(&["Read", "Bash"])` needs no `String`s.
- `UrlRefused` is `#[non_exhaustive]`; `AgentTurnGate`, `WebFetchNet`,
  `WebFetchTool`, `SkillToolOptions` and `BuiltinOptions` implement
  `Debug`.
- `WebFetchNet::new` takes the resolver itself (`impl Into<ResolveRef>`)
  and `unguarded` the client too (`impl Into<HttpClientRef>`):
  `WebFetchNet::unguarded(Arc::new(dns), Arc::new(client))` ->
  `WebFetchNet::unguarded(dns, client)`. New `ResolveRef`.
- `GenerateImageOptions`' fields are private:
  `GenerateImageOptions { base_url, http, ..GenerateImageOptions::new(key) }`
  -> `GenerateImageOptions::new(key).with_base_url(base_url).with_http(http)`,
  where `with_http` takes the client itself or a shared handle; read with
  `base_url()`, `model()` and `models()`. `GenerateImageTool` implements
  `Debug` and `Clone`.
- The Image API's helpers are the tool's own: `call_image_api`,
  `sniff_image_type`, `ImageCatalog`, `ImageError`, `ImageReply`,
  `ModelSettings` and `SettingSpec` are no longer public. A host calls the
  Image API through `GenerateImageTool`.
- `assert_public_url` takes the URL as text, so a host needs no `url`
  crate of its own: `assert_public_url(&Url::parse(s)?, &dns)` ->
  `assert_public_url(s, &dns)`. Text that is not a URL is the new
  `UrlRefused::Invalid`.
- `NoInbox` and `NoJobEvents` are `Clone`, `Copy`, `PartialEq` and `Eq`,
  as haest-core's do-nothing parts are.
- `media_output` takes the bytes, the media type and the name in the order
  `media_from_bytes` does, the context last:
  `media_output(label, bytes, media_type, ctx)` ->
  `media_output(bytes, media_type, label, ctx)`. Both text arguments are
  `&str`, so an old call still compiles: swap them by hand.
- `SubagentHub::spawn` gives a member's `max_concurrent` place back however
  it stops. A host that dropped the spawn (a timeout, a `select!`) while its
  `start_member` ran, a `start_member` that panicked, or a foreground spawn
  dropped while its member ran left the member Running for good: every later
  spawn in that conversation was refused with `TooMany`, and `running()`
  listed an agent that did not exist. A foreground member now runs on a task
  of its own, and dropping its spawn stops it as a cancelled caller does:
  its `MemberRun::end` hears `Ending::CallerCancelled` with its spend. A
  foreground spawn now needs a tokio runtime, as a background one did.

### haest-workspace

- `RealWorkspace::fs` and `SandboxWorkspace::fs` return `FsRef`, and
  `SandboxWorkspace::changes` an `Option<ChangeSetRef>`, as `Workspace`
  now has them. `RealFs` and the sandbox's file system implement
  `create_dir`, `create_dir_all`, `remove` and `remove_all`.
- `SandboxWorkspace::over` takes the files themselves, or a handle to them
  (`impl Into<FsRef>`), and `with_command` the command itself
  (`impl Into<ShellCommandRef>`):
  `SandboxWorkspace::over(Arc::new(MemFs::new())).with_command(Arc::new(Shout))`
  -> `SandboxWorkspace::over(MemFs::new()).with_command(Shout)`. A host that
  reads the files afterwards keeps an `FsRef` and hands over a clone.
- Removed `RealWorkspace::with_fs` and `SandboxWorkspace::overlay_changes`,
  which nothing called: the real workspace is always over `RealFs`, and the
  overlay's changes are `Workspace::changes()`.
- `RealWorkspace`, `SandboxWorkspace` and `OverlayChanges` implement
  `Debug` (a sandbox shows its commands and the names of its variables,
  never their values).
- `SandboxWorkspace::open` fails with an `OpenSandboxError`, which names
  the dir that did not open (the project or a mount) and keeps the I/O
  error as its source (breaking): `opening the sandbox: No such file or
  directory` -> `opening /work/project for the sandbox: No such file or
  directory`. `path()` and `kind()` read it, and `?` still turns it into
  an `io::Error` of the same kind.
- `SandboxWorkspace::with_commands` adds a list of commands in one call:
  `sandbox_commands().into_iter().fold(ws, SandboxWorkspace::with_command)`
  -> `ws.with_commands(sandbox_commands())`.

### haest-code

- `code_tool_names` takes `&[ToolRef]` (was `&[Arc<dyn Tool>]`), and it
  and `code_callable` take the allowed names as any strings
  (`&[impl AsRef<str>]`, was `&[String]`): `&FILE_TOOLS` passes as is.
- `CodeTool::new` takes the names as any strings and the tools themselves
  (`impl Into<ToolSourceRef>`):
  `CodeTool::new(names, Arc::new(tools))` -> `CodeTool::new(names, tools)`.
- `JsEnv` is built with `JsEnv::new(fs, cwd)` and `with_env`,
  `with_stdin`, `with_cancel` and `with_ends_at`; its fields are private,
  read with `fs()` and `cwd()`:
  `JsEnv { fs: Arc::new(mem), cwd, env, stdin: None, cancel, ends_at }` ->
  `JsEnv::new(mem, cwd).with_ends_at(at)`. `js_command` and `run_js` take
  their arguments as any strings.
- `VmSpec` is built with `VmSpec::new()` and `with_prelude`, `with_value`,
  `with_native_fn`, `with_modules` and `with_filename`; a native function
  is a plain async closure, and `NativeFn` and `NativeFuture` are gone:
  `spec.native_fns.insert(name, Arc::new(|args| async { .. }.boxed()))` ->
  `VmSpec::new().with_native_fn(name, |args| async { .. })`.
- `sandbox_commands` returns `Vec<ShellCommandRef>` (was
  `Vec<Arc<dyn ShellCommand>>`).
- `VmRun`, `VmBeat` and `VmStop` are `#[non_exhaustive]`; `CodeTool`,
  `JsEnv` and `VmSpec` implement `Debug` (a `JsEnv` shows the names of its
  variables, never their values).
- The Code tool hands its caller's `turn_time_left` on to the calls it
  makes.
- A tool the Code tool calls sees the caller's wind-down.
- `run_in_vm` takes no `dyn` and two fewer arguments (breaking): the caps
  are part of the spec (`VmSpec::with_caps`, read with `caps()`; the
  default is `VmCaps::default()`), and `on_beat` is any closure, `|_| {}`
  for none: `run_in_vm(code, &spec, caps, &cancel, Some(&|b| log(b)))` ->
  `run_in_vm(code, &spec.with_caps(caps), &cancel, |b| log(b))`, and
  `run_in_vm(code, &spec, caps, &cancel, None)` ->
  `run_in_vm(code, &spec.with_caps(caps), &cancel, |_| {})`. `VmSpec`'s
  `with_*` setters are `#[must_use]`.
- `JsEnv::with_env` takes a `ChildEnv`, as every other environment
  parameter in haest does (breaking):
  `.with_env(BTreeMap::from([("A".into(), "1".into())]))` ->
  `.with_env(ChildEnv::new([("A", "1")]))`. A name listed twice has its
  last value in `process.env`. `JsEnv` has a getter for each part it is
  built with: `env()`, `stdin()`, `cancel()` and `ends_at()` beside `fs()`
  and `cwd()`; its setters are `#[must_use]`.

### haest-mcp

- `McpHub::tools` returns `haest_core::Tools` (was `Vec<Arc<dyn Tool>>`).
- `McpHub` is a cheap handle: clones share the servers and failures, and
  the servers shut down when the last clone is dropped. Hold it, not an
  `Arc<McpHub>`.
- `McpHub::with_env` is a setter, and the log is set once with `with_log`
  instead of on every connect:
  `McpHub::with_env(env).connect(&servers, &|line| ..)` ->
  `McpHub::new().with_env(env).with_log(|line| ..).connect(&servers)`.
  Without a log the lines go nowhere; `failures()` has them either way.
- `McpHub::connect_transport` and the `rmcp` re-export are behind the new
  `rmcp` feature (off), so rmcp is not in haest-mcp's API unless a host
  asks: `haest-mcp = { .., features = ["rmcp"] }`.
  `hub.connect_transport(name, io, &|_| {})` -> `hub.connect_transport(name, io)`.
- `StdioTransport::with_env` is `with_env_var`: it adds one variable, and
  `with_env` means replacing a whole environment everywhere else.
- `McpFailure`'s fields are private: `failure.name` -> `failure.name()`,
  `report(&*failure.error)` -> `report(failure.error())`.
- `load_project_mcp_config` takes the variables as any closure (was
  `&dyn Fn`): `load_project_mcp_config(fs, cwd, &vars)` ->
  `load_project_mcp_config(fs, cwd, vars)`, with the closure's argument
  typed (`|name: &str| ..`).
- `McpStartError`, `McpConfigError` and `ProjectMcp` are
  `#[non_exhaustive]`; `McpHub` implements `Debug` (servers and failures
  by name, the environment's names but never its values).
- `McpTransport` is `#[non_exhaustive]` (breaking): a `match` adds a `_`
  arm, and haest adds a transport (`.mcp.json`'s `sse`) in a minor
  release.

### haest-mods

- `Mod::tools`, `Mods::tools` and `ModTools::wrap` take and return
  `haest_core::Tools` (was `Vec<Arc<dyn Tool>>`).
- `ModTools` forwards the wrapped tool's `Tool::on_resume`.
- `Mods` is a cheap handle whose list never changes once built: clones run
  the same mods for good, so hold it, not an `Arc<Mods>`. It is built with
  the new `ModsBuilder` (`Mods::builder`, `with`, `with_all`, `build`), and
  `Mods::add` is gone: on a handle already handed out it changed that clone
  only, so the runtime's tools and prompt never saw the mod.
  `let mut m = Mods::new(env); m.add(Arc::new(NoDotEnv)); let m = Arc::new(m);`
  -> `let m = Mods::builder(env).with(NoDotEnv).build();`. `with` takes
  the mod itself or an `Arc` of it the host keeps; the mods are held as the
  new `ModRef`. `Mods::new(env)` is a list of none.
- `ModOptions::with_mod` puts a host's own native mod in what `load_mods`
  loads: after the built-ins and the mod files, in the order added
  (`host_mods()` reads them back). It was `load_mods(..).await` then
  `Mods::add`.
- `ModTools::new`, `ModTools::wrap` and `ModSystemPrompt::new` take `Mods`,
  and the tool source and prompt themselves (`impl Into<ToolSourceRef>`,
  `impl Into<SystemPromptRef>`):
  `ModSystemPrompt::new(Arc::new(prompt), mods.clone())` ->
  `ModSystemPrompt::new(prompt, mods.clone())`;
  `ModTools::wrap(&Arc::new(mods), tools)` -> `ModTools::wrap(&mods, tools)`.
- `ModEnv` takes the host's `ModUi` itself (`impl Into<ModUiRef>`) and its
  fields are private, read with `cwd()`, `workspace()` and `ui()`:
  `ModEnv::new(cwd, mode, Arc::new(Terminal))` ->
  `ModEnv::new(cwd, mode, Terminal)`; `haest.workspace` ->
  `haest.workspace()`. `load_mods` takes the `ModUi` the same way:
  `load_mods(cfg, fs, Arc::new(ui))` -> `load_mods(cfg, fs, ui)`.
- `ModOptions`' fields are private, set with `with_user_mods(bool)`,
  `with_project_mods(bool)`, `with_approvals(bool)`, `with_auto`,
  `with_shell_tools`, `with_plan_listener`, `with_planning_tools` and
  `with_caps`, and read with getters of the same names:
  `cfg.user_mods = true; cfg.auto = Some(Arc::new(judge));` ->
  `ModOptions::new(..).with_user_mods(true).with_auto(judge)`, and
  `cfg.approvals = s.approvals` -> `.with_approvals(s.approvals)`.
- `ApprovalMod::new` takes nothing; auto mode's judge goes in with
  `with_auto` (held as the new `AutoJudgeRef`):
  `ApprovalMod::new(Some(Arc::new(judge)))` ->
  `ApprovalMod::new().with_auto(judge)`.
- `PlanMod::new` takes nothing and the `PlanListener` alias is gone; the
  listener is a closure given to `with_listener`:
  `PlanMod::new(Some(Arc::new(|on| ..)))` ->
  `PlanMod::new().with_listener(|on| ..)`.
- `ApprovalGate::with_lines` is `mod_ui` (`with_*` is only ever a setter),
  and `ApprovalGate::new` takes the prompter or an `Arc` of it (held as the
  new `PrompterRef`): `Arc::new(gate.with_lines(f))` -> `gate.mod_ui(f)`.
- `Mods`, `ModEnv`, `ModOptions`, `ModTools`, `ModSystemPrompt`,
  `ApprovalMod`, `ApprovalGate`, `PlanMod` and `JsMod` implement `Debug`
  (`Mods` lists its mods by name; a closure shows only whether it is set);
  `ApprovalMod`, `ModOptions`, `ModTools` and `ModSystemPrompt` are `Clone`.
- `ModLoadError` is `#[non_exhaustive]`: a `match` on it adds a `_` arm.
- A line to the human is info / warn / error / debug here too: new
  `ModUi::warn` (default: an `info` line), `ModLine::Warn`, and `ModEnv`
  forwards `warn`, `error` and `debug` beside `info`. `ModLine` is
  `#[non_exhaustive]`, with `text()` for whatever kind it is (breaking):
  a `match` in the function given to `ApprovalGate::mod_ui` adds an arm
  that still shows the line, `other => show(other.text())`.
- `CommandOutcome`, `BashVerdict` and `ApprovalAnswer` are
  `#[non_exhaustive]` (breaking): a mod command may come to ask the host
  for more, a judge to refuse outright, a human to allow for the session
  only. A `match` adds a `_` arm; building one is unchanged.

### haest-settings

- `SubagentCapSettings` gains `budgets` (the `subagent_budgets` setting,
  default off; the `claude` profile leaves it off).
- `Settings` gains `wind_down_at: WindDown` (bare: off).
- `turn_caps_for` fills `TurnCaps::context_window` with the window.
- `CapSettings` implements `Default`: the bare caps, the ones
  `Settings::bare` starts from. A host changing one cap writes
  `CapSettings { max_turn_usd: 5.0, ..CapSettings::default() }`.
- Both wall-clock caps are a `Duration`, the type haest-core's caps take
  (breaking): `CapSettings::max_turn_minutes: f64` ->
  `max_turn_time: Duration`, `SubagentCapSettings::minutes: u64` ->
  `time: Duration`. `caps.max_turn_minutes = 1.5` ->
  `caps.max_turn_time = Duration::from_secs(90)`. The settings keep their
  names and read minutes (`max_turn_minutes`, `subagent_minutes`).

### haest-config

- New setting `subagent_budgets`: `HAEST_SUBAGENT_BUDGETS`,
  `--subagent-budgets`, in docs/settings.md and docs/examples/config.toml.

- New settings, each off or at its old behaviour by default: `event_ring`
  (1000), `stdio`, `control_socket`, `control_dir`, `broker_url`,
  `broker_access`. New `ControlSocket`, `BrokerAccess` and
  `Config::control_dir()`; `Config` gains their fields (a struct literal
  breaks).
- docs/settings.md: a process-scoped setting's "Set in" reads "its variable
  or its flag only, never a config file".
- New setting `wind_down_at` (`HAEST_WIND_DOWN_AT`, `--wind-down-at`): a
  share from 0 to 1, 0 = off, the default; the `claude` profile leaves it
  off. A value above 1 is refused at load, naming its source.
- `Loader::env`, `Loader::flags` and `Loader::standard` take any pairs by
  value, `impl IntoIterator<Item = (K, V)>` with both `Into<String>`
  (breaking): a map moves in instead of being copied, and a literal needs
  no map. `Loader::standard(defaults, &env, &flags)` ->
  `Loader::standard(defaults, env, [("model", "x")])`; a
  `&HashMap<String, String>` still goes in as it is, a `&Vec` of pairs
  as `.iter().cloned()`. `Loader::xdg_dirs` takes pairs of anything
  `AsRef<str>` and reads only the two XDG values.
- `#[non_exhaustive]` on `ConfigError`, `SetError`, `Kind` and `Source`
  (breaking): a `match` adds a `_` arm, and haest adds a setting kind or a
  layer in a minor release.
- `ConfigError::Read` and `ConfigError::Toml` say what failed: `cannot read
  /x/config.toml` and `/x/config.toml is not valid TOML` (was the bare
  path), with where and why in the `source`.
- `Loaded::keys` is a `Keys` (new), not a `HashMap<String, String>`
  (breaking): a `KeySource` whose Debug output shows names only.
  `loaded.keys["OPENROUTER_API_KEY"]` -> `loaded.keys.get("OPENROUTER_API_KEY")`
  (an `Option<&str>`, None for an empty value); `.with_keys(loaded.keys)`
  takes it as it is.
- `Loaded` is `#[non_exhaustive]` (breaking): a host reads it and never
  builds it; `let Loaded { config, keys } = loaded;` ->
  `let Loaded { config, keys, .. } = loaded;`.
- New `mcp` feature, in `default`: a trusted project's `.mcp.json`, read
  into `Loaded::mcp_servers`. A settings loader with `default-features =
  false` builds no MCP client, so no rmcp, reqwest, rustls or ring (147
  crates in the tree become 49); `mcp = true` is then a note in
  `Loaded::notes`, and `Loaded::mcp_servers` is not there. Through the
  facade (breaking): `config` no longer turns on `mcp`, and the facade's
  `mcp` turns on the loader's. `features = ["config"]` with
  `default-features = false` -> `features = ["config", "mcp"]` to keep
  reading `.mcp.json` (148 crates become 50 without it).
- The crate docs show loading real files: `.load(&RealFs)` with
  haest-workspace's disk, which every example before read from memory.
- `Scope` and `EnvPassthrough` are `#[non_exhaustive]` (breaking): a
  `match` adds a `_` arm, as on `Kind` and `Source`. `ProjectFile` stays
  exhaustive (none, read, skipped is the whole set). `Loader` is `Clone`.
- `subagent_minutes` takes a fraction, as `max_turn_minutes` does
  (`2.5` is two and a half minutes; it was refused as not a whole number).
  A time too long for a `Duration` is the longest there is.
- A refused setting's error names the setting as a field, not only in its
  text (breaking): `ConfigError::Invalid { at, why }` ->
  `Invalid { at, key, why }`, and an unknown setting is its own variant,
  `UnknownSetting { at, key }`. `key` is the setting as the layer gives
  it, in a file its TOML path (`max_turn_steps`, `profiles.review.tools`,
  `env.GH_TOKEN`), so an editor or a settings screen can point at it
  without parsing the text.
- `ControlSocket` and `BrokerAccess` are `#[non_exhaustive]` (breaking): a
  `match` adds a `_` arm (the `haest` command's gives the least access,
  watch), and haest adds a level in a minor release as the transport's
  grants grow.

### haest-runtime

- How the conversation is assembled is the runtime's own (breaking):
  `RuntimeParent`, `WrappedTools`, `MainTools`, `RuntimePrompt`,
  `AgentPrompt`, `InboxSender`, `NoImageKey`, `own_tools`,
  `image_options`, `instruction_options`, `skill_options`,
  `workspace_for`, `jobs_for`, `connect_mcp`, `mods_for`, `inbox` and
  `session_usd` are crate-private. None worked without haest's session
  file, so "the pieces" never served a host that owns some parts. `build`
  is the way in, and the parts are read on the `Runtime`:
  `mods_for(&hooks, &*fs, &gate).await` -> `build(inputs, ui, prompter,
  opts).await?` then `rt.mods()`. A host that owns some parts builds its
  own `AgentDeps`, with `builtin_tools_where(|w| settings.tools.has(w),
  ..)` for haest's tools.
- `Runtime` keeps its fields private, each read with the getter of its
  name (breaking): `rt.hooks.flush().await` -> `rt.hooks().flush().await`,
  `rt.jobs.clone()` -> `rt.jobs().cloned()`, `rt.gate.revoke("Bash")` ->
  `rt.gate().revoke("Bash")`. A host running its own turn loop over the
  inbox takes `rt.inbox_mut()`. `Runtime` implements `Debug` (the host's
  UI left out).
- `build` takes the UI by value, as it takes the prompter (breaking):
  `build(inputs, Arc::new(Lines), DenyAll, opts)` ->
  `build(inputs, Lines, DenyAll, opts)`, and `rt.ui()` hands it back as
  the host's own type. A host that keeps its UI as well hands in an `Arc`
  of it: an `Arc` of a `Ui` is a `Ui`, and the runtime is then a
  `Runtime<Arc<TermUi>>`.
- `World` is a struct with two constructors (breaking):
  `World::Disk { child_env }` -> `World::disk(child_env)`,
  `World::Fs(Arc::new(MemFs::new()))` -> `World::files(MemFs::new())`.
  `World::fs` returns `&FsRef`; it made a new `Arc` of the real disk on
  every call. `World` implements `Debug`.
- `RuntimeInputs` keeps its fields private and holds handles (breaking):
  a part is set with its `with_*` setter and read with the getter of its
  name. `inputs.keys = Arc::new(|name: &str| ..)` ->
  `.with_keys(|name: &str| ..)`, `inputs.http = Arc::new(gateway)` ->
  `.with_http(gateway)`, `inputs.mcp_servers = vec![server]` ->
  `.with_mcp_servers([server])`, `inputs.routing = Some(scopes)` ->
  `.with_routing(scopes)` (the scopes themselves; leave it off for none);
  `keys()` is a `KeySourceRef` and
  `http()` an `HttpClientRef` (were `Arc<dyn KeySource>` and
  `Arc<dyn HttpClient>`). `RuntimeInputs` implements `Debug`, the custom
  providers by name only (an entry may hold a key).
- A runtime's services are handles, held by cloning them (breaking).
  `RuntimeHooks` is a cheap `Clone` (it was shared as an
  `Arc<RuntimeHooks>`), and the runtime holds `McpHub`, `SubagentHub`,
  `Option<JobHub>`, `TaskBoard` and an `FsRef` where it held an `Arc` of
  each: `let hooks: Arc<RuntimeHooks> = rt.hooks.clone()` ->
  `let hooks = rt.hooks().clone()`.
- The skills offered are core's `Skills` (was `Arc<Mutex<Vec<Skill>>>`):
  `*rt.skills.lock().unwrap() = found` -> `rt.skills().replace(found)`,
  and `rt.skills().list()` reads them.
- `RuntimeHooks` keeps its fields private: `hooks.settings` ->
  `hooks.settings()`. `hooks.ui` and `hooks.catalog` are gone (the host
  has its own UI; the catalog is `Catalog::builtin()`), and `new`,
  `store`, `remedies`, `window` and `compaction_for` are crate-private:
  hooks come from `build`, the session's id from `rt.session_id()`.
- `RuntimeHooks::compact_now` is crate-private, and its `auto: bool` an
  enum inside (breaking): it needed main's `&mut Agent`, which no host
  holds. `hooks.compact_now(&mut agent, &cancel, false)` ->
  `rt.compact(&cancel)`, the human's /compact.
- `#[non_exhaustive]` on `BuildError`, `SessionFileError`, `WorkspaceError`,
  `ShutDown` and `SessionInfo` (breaking): a `match` adds a `_` arm, and a
  host reads a `ShutDown` or a `SessionInfo` and never builds one.
- `SessionInfo::cwd` is a `PathBuf` (was `String`):
  `info.cwd == cwd.to_string_lossy()` -> `info.cwd == cwd`, and
  `format!("{}", info.cwd)` -> `format!("{}", info.cwd.display())`.
  `SessionInfo` implements `Eq`.
- Every public type implements `Debug` (`Runtime`, `RuntimeInputs` and
  `World` are new), and the crate warns on one that does not
  (`missing_debug_implementations`).
- Every doc example that can fail returns a `Result` and uses `?`; none
  writes an `Arc::new` or an `unwrap`. `list_sessions`' example lists a
  session it created, and `RuntimeInputs` has one of its own.
- `RuntimeHooks::append` takes the record by reference, as
  `SessionSink::append` does (breaking): `hooks.append(record)` ->
  `hooks.append(&record)`. `RuntimeInputs`' `with_*` setters are
  `#[must_use]`: a setter whose result is dropped set nothing.
- The runtime hands `RuntimeHooks` to `SharedDeps::with_services` as well
  as `with_hooks`; what tools publish, report and log reaches it as before.
- Feature `session` (off; the facade's `session` turns it on with
  `runtime`): `impl Driven for Runtime<U>`. The driver calls the runtime's
  own `next_agent_turn`, `run` and `shut_down`, and its mods are the
  driver's rewriter, so a session runs the turn loop a host without one
  runs, and a person's input steered into a turn goes through the mods
  too. New at the root: `HubUi` (a
  `Ui` whose lines are `notice` frames) and `subagent_frame`.
- `wind_down_at` reaches the conversation as wind-down thresholds, for main
  and every subagent of its hub.
- A turn's warning threshold reaches the human through `Ui::warn`.
- `Runtime::mods` returns `Mods`, now a cheap handle (was `Arc<Mods>`):
  `rt.mods().command(..)`.
- The session file's error is `SessionFileError`, and `BuildError`'s
  variant for it `SessionFile` (breaking), so the facade no longer has two
  `SessionError`s (haest-session's is a running session's):
  `BuildError::Session(SessionError::NotFound { .. })` ->
  `BuildError::SessionFile(SessionFileError::NotFound { .. })`.
  `Runtime::clear` returns it.
- `World::child_env` is a getter, the environment the disk was made with
  (breaking), and `World::starts_processes` says whether haest may start
  processes at all: `world.child_env(&ChildEnv::default()).is_some()` ->
  `world.starts_processes()`, and `world.child_env(&passed)` ->
  `world.child_env().map(|env| env.overlaid(&passed))`.
- `BuildOptions` says which model and effort a resume goes on with in an
  `OnResume` of two `Prefer`s, where it had two bools (breaking):
  `BuildOptions { resume, model_spec_given: true, effort_given: false }` ->
  `BuildOptions { resume, on_resume: OnResume { model: Prefer::Settings, effort: Prefer::Saved } }`.
  The session file is unchanged.
- `WorkspaceError::Sandbox` holds haest-workspace's `OpenSandboxError`
  (was an `io::Error`) and shows its message, so a sandbox that does not
  open names the dir (breaking only for a `match` that read the
  `io::Error`: `e.kind()` still works on it).
- `RuntimeInputs::with_mod` puts a host's own native mod in the
  conversation, after the built-ins and the mod files, whatever the `mods`
  settings say; `host_mods()` reads them back. A host had no way to: the
  `Mods` the runtime hands out was already shared, and a mod added to a
  clone of it reached nothing.
- `RuntimeInputs::with_models` takes the host's own `ModelResolver` (a
  closure, a test's `ScriptedProvider::resolver()`) in place of the
  built-in providers: every model spec resolves through it, so a test or
  an offline host runs a whole conversation without a key or a faked
  provider wire format. `models()` reads it back.
- `HubUi::hub` returns the stream its lines go into, so a host's own `Ui`
  wraps a `HubUi` for the lines and publishes its other frames beside them
  (the `haest` command's terminal does).
- `Resume` is `#[non_exhaustive]`: more ways to pick a session may come, so
  a `match` on it adds a `_` arm. Building one is unchanged.

### haest-ts

- `StripError` is `#[non_exhaustive]` (breaking): a `match` adds a `_` arm.

### haest-wire (new)

The format, serde only (no I/O, no tokio): `Envelope<B>` (`v`, `sid`,
`seq`, `ts`, `path`, the body flattened in), `Framer`, `Path`, `WIRE_V`,
`now_ms`; the wire's own frames `Outbound` (`Notice`, `Ask`, `Status`,
`ApprovalAsk`, `ApprovalWithdrawn`, `SubagentStarted`, `SubagentStopped`,
`Hello`, `Gap`, `Reply`) with `Access`, `Origin`, `Level`, `State` and
`Snapshot`; requests `Inbound { v, id, request }` with `Request` (`Turn`,
`Control`, `ApprovalAnswer`, `Snapshot`, `History`, `Events`), `Control`
and `Answer`. `decode` and `decode_value` check `v` before the shape
(`DecodeError`: `Json`, `Version`, `NoVersion`, `Shape`). `Ring` keeps
`capacity` frames outside the running turn and the running turn whole,
drops a model call's pieces once its answer is in, and reports a gap only
for an eviction (`Classify`, `FrameKind`, `Replay`). `LineDecoder` and
`encode_line` are NDJSON with a line cap (`LineTooLong`).

Changed since the first cut of these crates on `main` (breaking for Rust
code written against it). The JSON on the wire is unchanged, byte for byte:
`tests/wire_bytes.rs` pins every request, frame and word as the first cut
wrote them.

- `Request::Control` holds a `Control`, one variant per verb carrying only
  that verb's field, so `set_model` without a model or `interrupt` with an
  effort cannot be built: `Request::Control { verb: Verb::SetModelSpec,
  model_spec: Some(m), effort: None }` ->
  `Request::Control(Control::SetModel { model: m })`. `Verb` is gone. A
  `set_model` line without a `model` is refused as not a request
  (``missing field `model` ``) instead of by the transport (`set_model
  needs a model`); a field a verb does not use is still ignored.
- `Request::commands()` -> `Request::needs_command()`.
- `#[non_exhaustive]` on `Outbound`, `Request`, `Control`, `DecodeError`
  and `Level`: a `match` adds a `_` arm, so a new frame type, request, verb
  or level is additive in Rust as it already is on the wire.
- `Access`, `Origin`, `Level`, `State` and `Answer` have `as_str`,
  `Display` (a width pads it) and `FromStr`, and serde writes and reads the
  same word from the same list. An unknown word is the new `UnknownWord`:
  `unknown access "admin" (watch, command)`, also the cause of the
  `DecodeError::Shape` for a request that sends one.
- `Path` keeps its segments private: `path.0` -> `path.segments()` (`main`
  first). Every path starts with `main` by construction, and a line whose
  path does not is no longer read as a frame. `Path::main()` allocates
  nothing, where it allocated a `Vec` and a `String` for every frame.
- `Ring::push` takes the frame itself, or an `Arc` of one the caller also
  hands out: `ring.push(Arc::new(frame))` -> `ring.push(frame)`. `new`,
  `capacity`, `len`, `is_empty` and `since` no longer need `B: Classify`,
  and the new `set_capacity` changes what a ring keeps (a smaller capacity
  evicts at once, a gap like any eviction).
- `LineDecoder::with_limit` is a setter:
  `LineDecoder::with_limit(n)` -> `LineDecoder::new().with_limit(n)`;
  `LineDecoder::DEFAULT_LIMIT` is the 64 MiB. Its docs say what the limit
  bounds: the unfinished line kept between pushes, so a line that ends
  inside one push may pass it by up to that push's size.
- Every doc example returns a `Result` and uses `?`.
- `Outbound::Hello`'s `token` is a `Token`, so a hello's `Debug` (and a
  `Frame`'s, a `RunningSession`'s) no longer prints the broker's
  credential: `token: Some("t".to_string())` -> `token: Some("t".into())`,
  `token.as_deref()` -> `token.as_ref().map(Token::expose)`. On the wire it
  is the same plain string.
- `LineDecoder::limit` reads the limit back.
- `State` and `Answer` are `#[non_exhaustive]`: more turn states and
  approval answers may come, so a `match` adds a `_` arm (haest-session's
  prompter denies an answer it does not know). The words on the wire are
  unchanged.
- `LineDecoder` gives a line one way however it ends: the whitespace around
  it goes and a blank line is skipped, for a line ended by its newline as
  for the last one `flush` gives. A pushed line kept its spaces, and a line
  of spaces came out as a line, which then failed to parse as a frame.

### haest-session (new)

- `Hub` (`HubOptions { ring, buffer }`): one numbered stream. `subscribe(after)`
  gives a `Subscription`: the ring's catch-up, then live; one `gap` when it
  falls behind, and the turn never waits on it. `Frame` is
  `Envelope<Body>`, `Body` a loop event or a wire frame.
- `start(driven, hub)` runs the driver and returns a `SessionHandle`: one
  turn at a time; input that arrives mid-turn is steered into it; a person's
  input steered in but never read runs next as a person's turn; model,
  effort and clear wait for the running turn. `with`, `hold` (`Hold`),
  `send_input` (`PendingTurn`), `idle`, `settled`.
- `Backend` is the seam every transport serves: `meta`, `submit`, `turn`,
  `control`, `answer`, `subscribe`, `snapshot`, `history`, `close`, with
  `Input`, `Control` (`Interrupt`, `SetModelSpec`, `SetEffort`, `Clear`),
  `Meta`, `History` and `SessionError` (`Closed`, `Held`, `NoSuchAsk`,
  `Control`).
- `Driven` is the plug point the driver runs: `run`, `human_turn`,
  `next_agent_turn`, `take_unread`, `gate_agent`, `arrival`, `expecting`,
  `history`, `status` (`DrivenStatus`), `set_model_spec`, `set_effort`,
  `clear`, `close`. `impl Driven for Agent`. A change's error is a
  `BoxError`.
- Feature `approvals`: `SessionPrompter`, a `Prompter` whose asks are
  `approval_ask` frames a peer answers, under the runtime's `ApprovalGate`.

Changed since the first cut on `main` (breaking for Rust code written
against it; the wire JSON is unchanged):

- `Hub::new` takes the stream id alone, and a hub with options of its own
  is `Hub::new_with_options`: `Hub::new("s1", HubOptions::default())` ->
  `Hub::new("s1")`, and `Hub::new(sid, opts)` ->
  `Hub::new_with_options(sid, opts)`. The options are fixed when the hub
  is made, before any clone or subscriber exists; there is no setter that
  changes them for every clone, or that a subscriber made a no-op.
- `SessionHandle` drives itself: `turn`, `submit`, `control`, `answer`,
  `subscribe`, `snapshot`, `history`, `close` and `meta` are its own
  methods, and its `Backend` impl forwards to them, so a host no longer
  writes `use haest_session::Backend`. `submit` and `answer` never wait, so
  the handle's are not `async`: `session.answer(n, a).await` ->
  `session.answer(n, a)`; `meta` lends (`&Meta`) where the trait's returns
  one.
- `SessionHandle::with` says why it takes a closure returning a
  `BoxFuture`: an `AsyncFnOnce` cannot promise its future is `Send` on
  stable Rust, and the job runs on the driver's task.
- `#[non_exhaustive]` on `Control` and `SessionError`: a backend that
  matches a `Control` refuses one it does not know with a `_` arm.
- `SessionHandle` and `Hold` implement `Debug` (the handle shows its `Meta`
  and whether the session still runs).
- New `BackendRef`, the shared handle to a `Backend` (`plug_ref!`, as every
  other plug point): built from the backend itself, an `Arc` of one, or an
  `Arc<dyn Backend>`. Its `Debug` shows the stream's id.
- New plug point `RewritePrompt` (`RewritePromptRef`) and the defaulted
  `Driven::rewriter`: the driver runs every person's input through it,
  the input that starts a turn and input steered into one alike.
  `Driven::human_turn` now gets text the rewriter already ran on (breaking
  for a `Driven` that rewrote in `human_turn`): move the rewrite to
  `fn rewriter(&self) -> Option<RewritePromptRef> { Some(MyRewrite.into()) }`,
  or steered input skips it.
- `Hold` is `#[must_use]`: `session.hold().await?;` warns, since dropping
  the hold at once lets go of it.
- An approval ask after the hub closed is denied at once. It waited for
  the gate's timeout (10 minutes by default) or an interrupt, with no frame
  out for anyone to answer.

### haest-transport (new)

- `serve(backend, grant, start, source, sink)`: one connection's rules,
  the same on every transport. `hello` first; a `reply` per request with its
  `id`, a turn's when it ends and after its last frame; stream frames only as
  asked for with `events {after, follow}`; a request of another version or
  one the access forbids is refused with its `id`; a slow peer gets a `gap`,
  never a wait. `Grant` (`watch`, `command`), `Start`, `Ended`, the seams
  `LineSource` and `LineSink`, `NdjsonSource` and `NdjsonSink`, the trait
  `Transport` (`attach` returns an `Attached`: `ended`, `detach`; dropped is
  detached), `Peer` (the asking side) and `TransportError`.
- Feature `stdio`: `Stdio::new(read, write)` and `Stdio::process()` (stdin
  read on a thread of its own, `ProcessStdin`, so a closed conversation
  never waits on the parent at exit).
- Feature `unix` (Unix only): `UnixSocket::new(dir)` serves
  `<dir>/<stream id>.sock` with a discovery file beside it
  (`SocketDiscovery`); the dir is 0700, the socket and the file 0600; a
  crashed run's socket is replaced, a live one refused (`InUse`).
  `running_sessions(dir)` lists the sessions that answer (`RunningSession`)
  and removes a crashed run's files; `connect_session(dir, id)` reaches one
  by id or unique prefix (`UnixPeer`).
- Feature `broker`: `Broker::new(url).with_token(..).with_backoff(..)` dials out to a
  `ws://` or `wss://` broker. The token rides in `hello`, never in the URL,
  and nothing more goes until the broker's ack (`events` with the last seq
  it holds); the stream resumes there, so a frame lost in a closing socket is
  sent again, and a gap comes when the ring no longer holds what the broker
  missed. A dropped link is dialed again after a full-jitter `Backoff`
  (500 ms doubling to 30 s); close 4401 or 4408 (`AUTH_CLOSE`), or a 401 or
  403 handshake, ends it with `TransportError::Auth`.

Changed since the first cut on `main` (breaking for Rust code written
against it; the wire JSON is unchanged):

- `Transport::attach` and `serve` take `impl Into<BackendRef>`, so a host
  hands over its session as it is:
  `let backend: Arc<dyn Backend> = Arc::new(session.clone());
  UnixSocket::new(dir).attach(backend, grant)` ->
  `UnixSocket::new(dir).attach(session.clone(), grant)`. An `Arc` of any
  backend, or an `Arc<dyn Backend>`, still goes in as it is. A transport
  keeps one `BackendRef` and clones it per connection.
- Setters are `with_*`, as everywhere else in haest:
  `Broker::new(url).token(Some(t)).backoff(b)` ->
  `Broker::new(url).with_token(t).with_backoff(b)` (`with_token` takes a
  `String`, a `&str` or a `Token`); `Stdio::starting(start)` ->
  `Stdio::with_start(start)`. `Broker` has the getters `url`, `token` and
  `backoff`, `Stdio` has `start`.
- `Start::token` is an `Option<Token>` (haest-wire's), so `Start`,
  `Broker` and the hello all keep it out of `Debug` the same way:
  `token: Some("t".to_string())` -> `token: Some("t".into())`.
- `#[non_exhaustive]` on `Ended`: a `match` adds a `_` arm.
- `NdjsonSource`, `NdjsonSink`, `Peer` and `Stdio` implement `Debug`
  (counts and the start, never the pipes or a line's text).
- `TransportError::Path` says what failed on the path: it has a `what`
  verb, and displays `could not bind /run/haest/s1.sock` where it said the
  bare path. A request or discovery file that cannot be written as JSON is
  the new `TransportError::Encode`, no longer `NotAFrame`.
- `Peer::start`'s docs say `NotHello` for a first frame that is not a
  `hello` (`NotAFrame` only when the line is not a frame at all).
- `#![warn(missing_docs)]`, as the other crates have.
- `Attached` is `#[must_use]`: `UnixSocket::new(&dir).attach(s, g)?;`
  warns, since dropping it detaches the transport and removes the socket
  at once.
- `Stdio`, `UnixSocket` and `Broker` have `attach` of their own, so
  attaching one needs no `use haest_transport::Transport` (the trait
  forwards to them, for code generic over a transport). `Stdio::attach`
  cannot fail and returns the `Attached` itself (breaking):
  `let attached = Stdio::process().attach(s, g)?;` ->
  `let attached = Stdio::process().attach(s, g);`.
- `Ended` implements `Display` (`the peer left`, `the conversation ended`,
  `reading from the peer failed: ` and the error with its causes), so a
  host shows why a connection ended without Debug text; the `haest`
  command's "the connection ended" line uses it.
- `NdjsonSource::with_limit` sets the longest line it takes (64 MiB by
  default; `limit()` reads it), and the source keeps one read buffer
  where it allocated 16 KiB on every `next_line`, even with a line
  already waiting.
- One failed `accept` no longer ends a `UnixSocket` transport. A process
  out of file descriptors for a moment (EMFILE) dropped the listener and
  removed the socket and its discovery file, so nobody could attach to
  that conversation again after the pressure passed. The loop now pauses
  100 ms and accepts again; only the conversation's end stops it.

### haest-issues

- `issue_tools` and `issue_tools_named` return `haest_core::Tools` (was
  `Vec<Arc<dyn Tool>>`).
- `issue_tools` and `issue_tools_named` take the tracker itself, as
  `impl Into<TrackerRef>` (new: the handle the tools hold a tracker in), and
  `issue_tools_named` takes the noun first (breaking):
  `issue_tools(Arc::new(github))` -> `issue_tools(github)`,
  `issue_tools_named(Arc::new(board), "Card")` ->
  `issue_tools_named("Card", board)`. An `Arc` of a tracker still goes in.
- `CardTracker::new` takes the directory first and the file system as
  `impl Into<FsRef>` (breaking), the dependency last like every constructor
  in haest-providers: `CardTracker::new(Arc::new(fs), dir)` ->
  `CardTracker::new(dir, fs)`. `CardTracker` is `Clone`, the clones sharing
  one write lock, so `Arc<CardTracker>` -> `CardTracker` and `.clone()`; it
  is `Debug`, and `with_clock` takes `impl Into<ClockRef>`. New getter
  `dir()`.
- `GithubTracker::with_http` takes `impl Into<HttpClientRef>` (breaking):
  `.with_http(Arc::new(client))` -> `.with_http(client)`. `GithubTracker` is
  `Clone`.
- `TrackerError` is `#[non_exhaustive]` (breaking): a `match` adds a `_` arm.
- `Relation` is `#[non_exhaustive]` (breaking): a tracker that matches on
  one refuses, in a `_` arm, a relation it does not keep, and haest adds
  one (a duplicate) in a minor release. `LinkChange` stays exhaustive (add
  and remove are the whole set).
- New `reqwest` feature, on by default: the GitHub tracker's default client.
  With `default-features = false` no reqwest, rustls or ring is built (115
  crates in the tree become 38), and a tracker the host hands no client
  (`with_http`) fails every call, saying so.
- `CardTracker` and `GithubTracker` have every `Tracker` method inherent
  (`info`, `list`, `search`, `get`, `create`, `update`, `delete`, `link`):
  `tracker.get("42").await` needs no `use haest_issues::Tracker`.
- `GithubTracker::with_api_url` takes `impl Into<String>` like the other
  setters (was `&str`), and the tracker has the getters `owner()`,
  `repo()`, `api_url()`, `http()` and `has_token()` (the token itself is
  never read back). `CardTracker` gains `fs()` and `clock()`.
- The `issues` example's `main` returns a `Result` and uses `?` where it
  unwrapped.

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
