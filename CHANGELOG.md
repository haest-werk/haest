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
value itself, and holds it in a handle named after its trait. Read
"Upgrading" first.

### Upgrading

1. Plug points take the value itself, or an `Arc` of one the host already
   shares: `AgentDeps::new(Arc::new(models), Arc::new(tools),
   Arc::new("You are terse.".to_string()), cwd, Arc::new(workspace))` ->
   `AgentDeps::new(models, tools, "You are terse.", cwd, workspace)`.
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
   `TaskBoard::new(on_change).with_clock(clock)`, `board.create(task)` ->
   `board.create(task)?`, `ChildEnv::new([("K".into(), v)])` ->
   `ChildEnv::new([("K", v)])`, and `Arc<TaskBoard>` / `Arc<ToolDeferral>`
   -> `TaskBoard` / `ToolDeferral`.

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
  `ProviderRegistry::new(keys, ..)` and `AgentDeps::new(models,
  builtin_tools(BuiltinOptions::default()), "You are a careful engineer.",
  std::env::current_dir()?, RealWorkspace::new(env))`, in a `main` that
  returns a `Result`. The plug-point table lists `Tools`,
  `&'static str` and closures as prompts, and `ToolServices`.

- New example `durable_run` (`cargo run -p haest --example durable_run`,
  also under `cargo test`): the finished-run shortcut, and a kill and a
  resume on the memory store.

- The `notebook` example runs its helpers on haest's `SubagentHub` through
  `SubagentParent::start_member`: each helper on a node run of its own (its
  token, model spec and caps), closed by `MemberRun::end`. A new panel run
  puts a helper in the background: the person writes to it (`deliver_as`),
  the panel hears `SubagentEvent::Message`, its answer reaches main through
  `Inbox::submit_answer`, the run waits on `SubagentHub::settled`, and every
  helper reads the notebook's own note.

- New features, none in `default`: `wire` (`haest::wire`), `session`
  (`haest::session`; with `runtime`, the runtime behind a session; with
  `mods`, approval asks answered over the session), `transport`
  (`haest::transport`), `transport-stdio`, `transport-unix` and
  `transport-broker` (pulls a WebSocket client on rustls).
- New examples, each run by `cargo test`: `wire`, `session`, `stdio`,
  `control_socket` and `broker`.
- New examples, both run by `cargo test`: `threshold` (a host's own
  thresholds: remind the model at 60% of the window, warn the human at the
  third model call) and `wind_down` (`wind_down_at` with a host's
  subagents).

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
- `AgentDeps::new(models, tools, system_prompt, cwd, workspace)` takes each
  part as `impl Into<..Ref>` (`cwd` as `impl Into<PathBuf>`), and so does
  `SharedDeps::new(models, cwd, workspace)`:
  `AgentDeps::new(Arc::new(m), Arc::new(t), Arc::new(p), cwd, Arc::new(w))`
  -> `AgentDeps::new(m, t, p, cwd, w)`.
- `AgentDeps`'s fields are private: `deps.tools` -> `deps.tools()` and
  `with_tools(..)`, `deps.system_prompt` -> `system_prompt()` and
  `with_system_prompt(..)`, `deps.shared` -> `shared()` and
  `with_shared(..)`. New `AgentDeps::from_shared(shared, tools, prompt)`: a
  subagent's deps from its parent's. Every `SharedDeps::with_*` is on
  `AgentDeps` too.
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
- `JsonlSessionFile::new` and `JsonlSessionFile::with_error_handler` take
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
  tools get another. `TaskBoard::with_clock(on_change, clock)` ->
  `TaskBoard::new(on_change).with_clock(clock)`, the clock as
  `impl Into<ClockRef>`.
- `deferred_tools::ToolDeferral` is a cheap `Clone` whose clones share what
  is loaded: `Arc<ToolDeferral>` -> `ToolDeferral`.

`Fs` splits its flags (breaking):

- `Fs::mkdir(path, recursive)` -> `Fs::create_dir(path)` and
  `Fs::create_dir_all(path)`; `Fs::remove(path, recursive)` ->
  `Fs::remove(path)` and `Fs::remove_all(path)`. An `Fs` implements
  `create_dir` and `remove`; `create_dir_all` and `remove_all` have defaults
  built on them, and `remove_all` removes a link without walking into it.
  `MemFs`, `ShutFs` and every `Fs` in the workspace implement all four.

Room to grow (breaking):

- `#[non_exhaustive]` on `LoopEvent`, `ProviderEvent`, `ToolError`,
  `InnerCallEvent`, `ModelError`, `run::RunError`, `CompactionError`,
  `MediaError`, `TaskError`, `SessionRecord` and `StopReason` (providers
  keep adding stop words): a `match` outside haest-core adds `_ => ..`, and
  haest adds a variant in a minor release.
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
- A threshold goes in as itself: `Thresholds::with(impl Into<ThresholdRef>)`
  and `Thresholds::new(impl IntoIterator<Item = impl Into<ThresholdRef>>)`
  (`.with(SaveNotes)?`); `Site::defaults`, `WindDown::thresholds` and
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
- New `Ledger::with_thresholds(Thresholds<LedgerSite>)`: the ledger's
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
- `JsonlSessionFile::new` and `with_error_handler` take `data_dir: impl
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
- Every doc example's `main` returns a `Result` and uses `?`; `unwrap` and
  `expect` are left only inside asserts and on locks. The docs name an item
  the prelude holds by its root path (`haest_core::Agent`), everything else
  by its module path (`haest_core::run::RunKey`). `Ledger::reserve`
  documents its errors.

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

### haest-providers

- `ProviderRegistry::new` takes only the keys, as `impl Into<KeySourceRef>`
  (an `Arc<dyn KeySource>` the host already shares still goes in as it is);
  providers.json's entries and the options come by setter (breaking):
  `ProviderRegistry::new(Arc::new(keys), custom, opts)` ->
  `ProviderRegistry::new(keys).with_custom(custom).with_options(opts)`, and
  `.with_http(client)` for the common case.
- `ProviderRegistry::resolve` is inherent: `registry.resolve(spec)` needs no
  `use haest_core::ModelResolver`. The registry is still a `ModelResolver`.
  It is `Clone` and `Debug`: clones share what was built, so
  `Arc::new(registry)` -> `registry.clone()`; a `with_*` setter starts a
  cache of its own. Getters `keys()`, `custom()`, `options()`.
- `RegistryOptions` keeps its fields private (breaking): set a part with its
  `with_*` setter, read it with the getter of its name.
  `RegistryOptions { http: Arc::new(client), routing, ..Default::default() }`
  -> `RegistryOptions::default().with_http(client).with_routing(routing)`
  (also `with_retry`, `with_headers_timeout`, `with_app_title`,
  `without_app_title`, `with_app_url`). `with_routing` takes the scopes or
  an `Option` of them.
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

### haest-tools

- Tool lists are `haest_core::Tools` and single tools `ToolRef`:
  `builtin_tools`, `builtin_tools_where`, `agent_tools`, `agent_tools_with`,
  `job_tools`, `task_tools`, `SubagentHub::tools_for` and
  `SubagentParent::{base_tools, subagent_tools, tool_source}` take or return
  `Tools` (was `Vec<Arc<dyn Tool>>`), `SubagentParent::system_prompt` takes
  `&[ToolRef]`, `skill_tool` returns `ToolRef`. `JobHub::start` takes
  `impl Into<WorkspaceRef>`.
- A subagent's lent services are their own wrapper (`ChildServices`), beside
  its lent hooks, now that `ToolServices` is a slot of its own.
- New `SubagentParent::start_member(&MemberStart, &CancellationToken)`,
  default `InProcessRun` (the member haest has always run). It runs before
  a member's loop exists: an `Err` refuses the spawn (the name is freed,
  nothing runs, no end follows), an `Ok` hands back the member's
  `MemberRun`.
- New `MemberRun`, every method a default: `model_spec()`, `caps()`,
  `deps() -> Option<SharedDeps>` (used as given, hooks included) and
  `end(MemberEnd)`, awaited once after the member's turn and before its
  answer goes back. New `InProcessRun`, `MemberStart`, `MemberEnd`,
  `MemberCaps`, `Ending` and `Spend`; `MemberStart`, `MemberEnd` and
  `Ending` are `#[non_exhaustive]`.
- `subagent_note` takes `&MemberCaps` (was `&SubagentCaps`): pass
  `&MemberCaps::from(caps)`.
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
  both `Option<SubagentBudget>`; a literal written out in full adds `None`.
  New `SubagentBudget { usd, time }`.
- `AgentError` gains `OverBudget { asked, available }` and
  `OverTime { asked, left, setting }`, and no longer derives `Eq`.
- New `SubagentHub::deliver_as(Sender, to, text)` and
  `Sender::{Agent, Person}` (`#[non_exhaustive]`): the person writes to a
  running subagent, or to main, under "[Message from <name>, the person you
  work for]". SendMessage is `Sender::Agent` and reads as before.
- `SubagentEvent` gains `Message { from: Sender, to, text }`, through
  `on_subagent` for every delivery (a refused one fires nothing).
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

### haest-workspace

- `RealWorkspace::fs` and `SandboxWorkspace::fs` return `FsRef`, and
  `SandboxWorkspace::changes` an `Option<ChangeSetRef>`, as `Workspace`
  now has them. `RealFs` and the sandbox's file system implement
  `create_dir`, `create_dir_all`, `remove` and `remove_all`.

### haest-code

- `code_tool_names` takes `&[ToolRef]` (was `&[Arc<dyn Tool>]`).
- `JsEnv::fs` is an `FsRef` (was `Arc<dyn Fs>`): `fs: Arc::new(mem)` ->
  `fs: FsRef::new(mem)` or `fs: mem.into()`.
- The Code tool hands its caller's `turn_time_left` on to the calls it
  makes.
- A tool the Code tool calls sees the caller's wind-down.

### haest-mcp

- `McpHub::tools` returns `haest_core::Tools` (was `Vec<Arc<dyn Tool>>`).

### haest-mods

- `Mod::tools`, `Mods::tools` and `ModTools::wrap` take and return
  `haest_core::Tools` (was `Vec<Arc<dyn Tool>>`).
- `ModTools` forwards the wrapped tool's `Tool::on_resume`.

### haest-settings

- `SubagentCapSettings` gains `budgets` (the `subagent_budgets` setting,
  default off; the `claude` profile leaves it off).
- `Settings` gains `wind_down_at: WindDown` (bare: off).
- `turn_caps_for` fills `TurnCaps::context_window` with the window.

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

### haest-runtime

- The runtime hands `RuntimeHooks` to `SharedDeps::with_services` as well
  as `with_hooks`; what tools publish, report and log reaches it as before.
  `WrappedTools::new` takes `Tools`.
- Feature `session` (off; the facade's `session` turns it on with
  `runtime`): `impl Driven for Runtime<U>`. The driver calls the runtime's
  own `human_turn`, `next_agent_turn`, `run` and `shut_down`, so a session
  runs the turn loop a host without one runs. New at the root: `HubUi` (a
  `Ui` whose lines are `notice` frames) and `subagent_frame`.
- `wind_down_at` reaches the conversation as wind-down thresholds, for main
  and every subagent of its hub.
- A turn's warning threshold reaches the human through `Ui::warn`.

### haest-wire (new)

The format, serde only (no I/O, no tokio): `Envelope<B>` (`v`, `sid`,
`seq`, `ts`, `path`, the body flattened in), `Framer`, `Path`, `WIRE_V`,
`now_ms`; the wire's own frames `Outbound` (`Notice`, `Ask`, `Status`,
`ApprovalAsk`, `ApprovalWithdrawn`, `SubagentStarted`, `SubagentStopped`,
`Hello`, `Gap`, `Reply`) with `Access`, `Origin`, `Level`, `State` and
`Snapshot`; requests `Inbound { v, id, request }` with `Request` (`Turn`,
`Control`, `ApprovalAnswer`, `Snapshot`, `History`, `Events`), `Verb` and
`Answer`. `decode` and `decode_value` check `v` before the shape
(`DecodeError`: `Json`, `Version`, `NoVersion`, `Shape`). `Ring` keeps
`capacity` frames outside the running turn and the running turn whole,
drops a model call's pieces once its answer is in, and reports a gap only
for an eviction (`Classify`, `FrameKind`, `Replay`). `LineDecoder` and
`encode_line` are NDJSON with a line cap (`LineTooLong`).

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
- Feature `broker`: `Broker::new(url).token(..).backoff(..)` dials out to a
  `ws://` or `wss://` broker. The token rides in `hello`, never in the URL,
  and nothing more goes until the broker's ack (`events` with the last seq
  it holds); the stream resumes there, so a frame lost in a closing socket is
  sent again, and a gap comes when the ring no longer holds what the broker
  missed. A dropped link is dialed again after a full-jitter `Backoff`
  (500 ms doubling to 30 s); close 4401 or 4408 (`AUTH_CLOSE`), or a 401 or
  403 handshake, ends it with `TransportError::Auth`.

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
