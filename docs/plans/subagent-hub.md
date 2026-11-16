# One subagent hub (plan + seam, opt-in)

Status: built on this branch, 2026-11-17: the seam, budgets (G3) and the five follow-ups
(G-H1 to G-H5), so brain can drop its own `Hub`. Decided by the owner: the subagent hub lives in haest, and
a host's own hub (brain's `Hub` in `node/src/web/agent/run/agents.rs`) shrinks to an
implementation of a seam on haest's side. Names follow haest 0.2.0 (`main` at `d05f75e`) plus
this branch; every haest-tools item is at the crate root (`haest_tools::MemberRun`), every cap
type in `haest_core::caps`. Brain's copy still compiles against an older haest
(`AgentDirectory` returning `Result<_, String>`, `AgentToolOptions::models`); the rows below
use today's names.

## What brain's hub does that `SubagentHub` + `SubagentParent` cannot say today

brain runs every helper as its OWN node run: the node starts it (its token, model, caps), the
helper's loop calls models with that token and logs as that run, and the node closes it (its
messages flushed first, its cost summed). haest's hub builds every member from the parent's
`deps()` and tells the parent about it only through two synchronous callbacks
(`on_activity`, `on_subagent`).

| Code | brain's hub | haest 0.2.0 |
|---|---|---|
| B1 | `HubHost::start_run(name, model, stop)`: an async start per helper, BEFORE its loop exists; it can refuse (the model reads why) and returns the helper's run | nothing: `deps()` is synchronous and the same for every member; no refusal |
| B2 | a name is held while the run starts and counts toward `max_running`; a refused start frees it (it never ran, so it is not "finished") | the name is taken in `Members::reserve` and only `leave` frees it (into `finished`) |
| B3 | the started run decides the helper's model (the node maps "haiku" to an id) | `input.model_spec`, else `default_model()` |
| B4 | the started run's limits are the helper's caps, in its note too (`NoteCaps`) | every member gets the hub's `SubagentCaps`, note included |
| B5 | `HubHost::new_agent(name, run, tools, prompt)`: the helper's loop over ITS run (models through its token, its own record log, the run's pause gate, events tagged with its name) | the parent's deps, hooks wrapped in `ChildHooks` (no `on_message`, no `before_model_call`) |
| B6 | `HubHost::stop_run(run, reason, answer)` after the turn, awaited BEFORE the answer goes back: flush, close the run, read its cost and model calls | `on_subagent(Stopped { ok })`: synchronous, no answer, no spend, no handle, no reason beyond `ok` |
| B7 | `Event::AgentStart { agent, parent, description, prompt, model, kind, background }` | `SubagentEvent::Started { name, parent, description }` |
| B8 | `Event::AgentEnd { agent, stop_reason, cost_usd, calls, answer }`, reason `done` / `stopped` / the loop's | `Stopped { ok }` |
| B9 | `Hub::started()`: helpers started in this run; main's caps become the tree's once one started (`tree_limits`) | no count |
| B10 | `Event::AgentMessage` for every delivery, `person: true` for the person's | `deliver` tells nobody |
| B11 | `Hub::from_person`: the person writes to a running helper (or main) as "[Message from <name>, the person you work for]" | `deliver(from, ..)` always says "agent \"<from>\"" |
| B12 | `HubHost::to_main(text, message)`: a message (main hears it now) vs a background answer (held until every helper is done: `MainInbox`) | `Inbox::submit(text)`: one kind |
| B13 | `Hub::idle()`, `Hub::settled(ms)` on a `watch` pulse: nothing running or starting, no answer on its way | `answers_due()` and `running()`, polled; no signal |
| B14 | its own note text (mentions "the person", adds an explore line) | `subagent_note` is fixed |
| B15 | the run's depth offset (`me.depth + caps.depth < caps.max_depth`): a helper run's own hub starts deeper | `me.depth < caps.max_depth` |

Already the same: the post office (`deliver`, `stop`, `stop_all`, finished names refused,
unread messages appended to the answer), unique names, the background hand-back (to the
starter, else main), a crash as the answer, kinds of subagent (`agent_tool_options`,
`subagent_tools`: W6), the depth and breadth caps.

## The seam

Default methods only: a parent that implements none of it builds, runs and reads exactly as
before.

```rust
#[async_trait]
pub trait SubagentParent: Inbox {
    // ... as today ...
    /// A member is about to start: run it here (the default), or elsewhere, or refuse.
    async fn start_member(
        &self,
        start: &MemberStart,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn MemberRun>, AgentError> {
        Ok(Box::new(InProcessRun))
    }
    /// The note under the member's system prompt; `member` has its run's model spec and caps.
    fn subagent_note(&self, member: &MemberStart) -> String { haest's }   // B14, G-H5
}

pub trait Inbox {
    fn submit(&self, text: String);                                       // a message for main
    fn submit_answer(&self, text: String) { self.submit(text) }           // B12, G-H3
}

impl SubagentHub {
    pub fn deliver_as(&self, sender: Sender, to: &str, text: &str)        // B11, G-H1
        -> Result<String, AgentError>;
    pub fn idle(&self) -> bool;                                           // B13, G-H4
    pub async fn settled(&self, timeout: Duration) -> bool;
}

#[non_exhaustive] pub enum Sender { Agent(String), Person(String) }
#[non_exhaustive] pub enum SubagentEvent { .., Message { from: Sender, to: String, text: String } }  // B10, G-H2

/// A started member's run: what the host decided about it, and its end.
#[async_trait]
pub trait MemberRun: Send + Sync {
    fn model_spec(&self) -> Option<String> { None }  // B3
    fn caps(&self) -> Option<MemberCaps> { None }    // B4
    fn deps(&self) -> Option<SharedDeps> { None }    // B5
    async fn end(self: Box<Self>, end: MemberEnd) {} // B6
}

pub struct MemberStart { name, id, parent, depth, input: SpawnInput, model_spec, caps: MemberCaps }
pub struct MemberEnd { ending: Ending, answer: String, spend: Spend }
#[non_exhaustive]
pub enum Ending { Answered, Stopped, CallerCancelled, Capped(String), Failed(String), Crashed(String) }
pub struct Spend { usage: Usage, model_calls: u32 }   // .usd()
pub struct MemberCaps { usd: f64, steps: u32, time: Duration }
```

A spawn, in order:

1. the tool clamps the `budget` (G3, when on) and checks its time fits the caller's turn;
2. the hub clamps the budget again (any `AgentDirectory` caller) and holds its USD on the
   conversation ledger (G3, when on); a hold that does not fit refuses the spawn before a
   name is taken;
3. the hub checks depth and breadth and takes the name (B2: it counts toward `max_concurrent` from here);
   a refusal here drops the hold;
4. `start_member`, awaited. An `Err` is the spawn's refusal, as is: the name is freed (never
   "finished": nothing ran), the hold released, `end` never called;
5. the agent is built from the handle: its `deps()` (else the parent's), its `model_spec()`
   (else the call's, else the default), its `caps()` (else `MemberStart::caps`, applied over
   the deps' `turn_caps`);
6. the turn runs; `on_subagent` sees `Started`, every `Loop` event, `Stopped` as today;
7. it leaves the table (a message for it is refused from here), late messages are taken;
8. `end`, awaited, with how it ended, the answer as the caller gets it, and the spend;
9. the hold is released; the answer goes back (foreground) or is handed back (background,
   still counted in `answers_due` until then): steered into a running starter, else
   `Inbox::submit_answer`;
10. the spawn closes: from the call until here (a refused one until its refusal) the hub is
   not `idle()`, and `settled` wakes as each one closes.

### Why this shape

- **`end` lives on the handle, not on `SubagentParent`.** The handle is the host's per-member
  state (brain's `GatewayRun`, its record log). On the parent, `end_member(run, ..)` would need
  the parent to keep a map by id (brain's `logs: Mutex<HashMap<..>>`) or downcast an `Any`.
  Taking `self: Box<Self>` makes "end exactly once" a type fact: the hub gives the handle up.
- **The handle overrides `deps`, not single fields.** brain's helper differs in its models
  (its token), its hooks (its log, the pause gate, tagged events) and nothing else of note; a
  host that builds a run elsewhere builds its `SharedDeps` whole, as `SubagentParent::deps`
  does for the conversation. Own deps are used as they are,
  hooks included: the default member's `ChildHooks` (no persistence, inner calls to
  `on_subagent`) are for a member that borrows the parent's hooks. The hub still sets the
  member's tools, prompt (parent's + note) and turn caps.
- **`end` returns nothing.** The spend is the loop's own count (`TurnEnd`'s usage, every
  `ModelStart`). brain's `stop_run` returned the node's sum of the SAME calls (the gateway
  reports each call's cost in its usage), so nothing needs overriding; the Agent call's
  `child_cost_usd` stays the loop's figure.
- **Start gets the caller's turn token, and is never dropped mid-flight.** A host's start may
  have side effects (a node run); dropping the future could leak one. The host stops its own
  start when the token fires and returns its refusal (brain's `call_within(.., Some(stop))`).
  A member stopped while starting (StopAgent) runs nothing: its turn aborts at once and `end`
  gets `Stopped`.
- **Refusal is a plain `AgentError`.** A host's own reason is `AgentError::Refused(text)`; it
  may return any variant (`TooMany` when its own pool is full).

## brain's hub piece -> haest's

| brain (`run/agents.rs`, `run/chat.rs`) | haest | Notes |
|---|---|---|
| `HubHost` | `SubagentParent` (+ `Inbox`) | |
| `HubHost::start_run(name, model, stop)` | `SubagentParent::start_member(&MemberStart, cancel)` | `MemberStart::{name, input.model_spec, ..}`; `Err(String)` -> `Err(AgentError::Refused(text))` |
| `GatewayRun` | the host's `impl MemberRun` holding it | |
| `run.model` | `MemberRun::model_spec()` | |
| `NoteCaps` from `run.limits` | `MemberRun::caps() -> MemberCaps` | the note and the member's turn caps both read it |
| `HubHost::new_agent(name, run, tools, prompt)` | `MemberRun::deps() -> Option<SharedDeps>` | the hub supplies tools and prompt; drop brain's own Agent construction |
| `HubHost::stop_run(run, reason, answer) -> (cost, calls)` | `MemberRun::end(MemberEnd)` | reason: `Ending` (`Answered` = "done", `Stopped` and `CallerCancelled` = "stopped"); cost: `spend.usd()`; calls: `spend.model_calls` |
| `Event::AgentStart` | emitted from `start_member` (or `on_subagent(Started)`) | prompt, kind, background in `MemberStart::input`, model from its own run |
| `Event::AgentEnd` | emitted from `MemberRun::end` | |
| `Hub::started()`, `tree_limits` | a counter the host bumps in `start_member` | |
| `HubHost::base_tools`, `subagent_tools(base, kind)`, `MAIN_ONLY`, `EXPLORE_TOOLS` | `SubagentParent::subagent_tools(kind)` | haest drops the task tools itself |
| `subagent_types()`, `model_choice` | `SubagentParent::agent_tool_options()` | |
| `HubHost::system_prompt(run, read_only)` | `SubagentParent::system_prompt(model_spec, tools)` | read-only = no `Edit` in `tools`; the model's inputs from `ModelFacts::input_modalities(model_spec)` |
| `HubHost::to_main(text, true)` (a message) | `Inbox::submit(text)` | steering main when it runs stays the host's (as `ChatHost::to_main` does) |
| `HubHost::to_main(text, false)` (a helper's answer), `MainInbox::push` | `Inbox::submit_answer(text)` (G-H3) | `MainInbox` itself stays brain's (it is chat.rs's continuation policy); its `next` waits on `SubagentHub::settled` instead of `hub.pulse` |
| `HubHost::tell` + `Watchers` + `drive` (a helper's tagged events) | `SubagentParent::on_subagent(id, Loop(ev))` | |
| `Table`, `Member`, `Status`, `slug`, `reserved` | `Members` (hub-private) | |
| `Hub::spawn` / `deliver` / `stop` / `stop_all` / `running` | `SubagentHub`'s | |
| `Hub::run_member` (own task, crash = answer) | hub-private `run_member` + `drive` (`catch_unwind`) | |
| `Hub::hand_back` | hub-private `hand_back` | |
| `Hub.caps.depth` offset | `SubagentCaps::max_depth = max_depth - depth` | B15 |
| `Hub::from_person(asked_by, to, text)` | `SubagentHub::deliver_as(Sender::Person(asked_by), to, text)` (G-H1) | the receiver reads `[Message from <asked_by>, the person you work for]`, as brain's |
| `Event::AgentMessage { from, to, text, person }` | `SubagentParent::on_subagent(id, SubagentEvent::Message { from: Sender, to, text })` (G-H2) | `person` = `Sender::Person`; fired for every delivery, nothing for a refused one |
| `Hub::idle`, `Hub::settled(ms)`, `pulse` | `SubagentHub::idle()`, `SubagentHub::settled(Duration) -> bool` (G-H4) | true = idle, false = timed out; `Duration::MAX` waits as long as it takes |
| `subagent_note(.., caps, kind)` | `SubagentParent::subagent_note(&MemberStart) -> String` (G-H5) | brain's own text, explore line included: `member.caps` are the run's, `member.input.subagent_type` the kind |
| `HUBS`, `register`, `message_agent` | stays brain's (its registry by run id) | `message_agent` calls `deliver_as` |

## The follow-ups, as built

| Code | Gap | Built | Differs from the proposal |
|---|---|---|---|
| G-H1 | the person writing to a member (B11) | `SubagentHub::deliver_as(sender: Sender, to, text)`, `#[non_exhaustive] Sender::{Agent(name), Person(name)}`; `AgentDirectory::deliver` = `Agent` | no |
| G-H2 | a delivery event (B10) | `SubagentEvent::Message { from: Sender, to, text }` through `on_subagent`, keyed by the receiving member's id; one for main by the sending member's, the person's to main by `"main"`. `SubagentEvent` and `Ending` are now `#[non_exhaustive]` | `from: Sender` instead of `from: String` + `person: bool`: the sender's kind is the variant, and a third kind of sender would not need a second flag |
| G-H3 | message vs answer for main (B12) | `Inbox::submit_answer(text)`, default `submit` | no |
| G-H4 | idle signal (B13) | `SubagentHub::idle()`, `settled(timeout) -> bool` on a `watch` bumped as each spawn closes | the hub counts every spawn from the call until its answer is back (starting, running, its `end`, the hand-back), not members plus `answers_due`: one counter, and a member's `end` is inside it. `settled` says whether it settled |
| G-H5 | the host's own note (B14) | `SubagentParent::subagent_note(&MemberStart) -> String`, default haest's `subagent_note` | returns `String`, not `Option<String>`: the default IS haest's text, and a parent that only adds a line calls `haest_tools::subagent_note` itself. `member` carries the run's own model spec and caps (`MemberRun::model_spec`, `caps`), so the note says what the member really runs under |

Durable subagents (`durable-runs.md` R6) build on `MemberRun::deps` (a member's own sink)
and `end` (its record closed).

## Budgets (G3) on the hub

`budgets.md` section S, built on this branch after the seam:

- `Ledger::{reserve, release, available, held}`: holds on the conversation's USD;
  available = cap - spent - held, floored at 0, None with no USD cap. A hold is released
  exactly once (`release` takes it by value).
- The Agent tool's `budget {usd, minutes}`, offered only when `AgentToolOptions::budget_caps`
  is Some; `SubagentHub::tools_for` sets it from its caps when `SubagentCaps::budgets` is on
  (the `subagent_budgets` setting, `SubagentCapSettings::budgets`, default off; the `claude`
  profile leaves it off).
- Each value only tightens (an absent one = the cap); `SpawnInput::budget` carries the
  clamped values to any `AgentDirectory`.
- The time must fit what is left of the caller's turn (`ToolContext::turn_time_left`, filled
  by the loop from its guard). Q6 (refuse or shrink) is decided in ONE function,
  `fit_time` in haest-tools' `agents/budget.rs`: refuse, with what is left.
- The hub holds the USD (step 2 above) and runs the member under the budget's caps. A child
  with no cap (the cap is 0 and none asked) does not fit under a capped turn or ledger: the
  model is told what is left and names an amount.
