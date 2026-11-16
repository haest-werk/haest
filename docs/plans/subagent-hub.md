# One subagent hub (plan + seam, opt-in)

Status: plan and seam, 2026-11-16. Decided by the owner: the subagent hub lives in haest, and
a host's own hub (brain's `Hub` in `node/src/web/agent/run/agents.rs`) shrinks to an
implementation of a seam on haest's side. Names follow `main` at `1c7372a` (0.2.0) plus this
branch. Brain's copy still compiles against an older haest (`AgentDirectory` returning
`Result<_, String>`, `AgentToolOptions::models`); the rows below use today's names.

## What brain's hub does that `SubagentHub` + `SubagentParent` cannot say today

brain runs every helper as its OWN node run: the node starts it (its token, model, caps), the
helper's loop calls models with that token and logs as that run, and the node closes it (its
messages flushed first, its cost summed). haest's hub builds every member from the parent's
`deps()` and tells the parent about it only through two synchronous callbacks
(`on_activity`, `on_subagent`).

| Code | brain's hub | haest at `1c7372a` |
|---|---|---|
| B1 | `HubHost::start_run(name, model, stop)`: an async start per helper, BEFORE its loop exists; it can refuse (the model reads why) and returns the helper's run | nothing: `deps()` is synchronous and the same for every member; no refusal |
| B2 | a name is held while the run starts and counts toward `max_running`; a refused start frees it (it never ran, so it is not "finished") | the name is taken in `Members::reserve` and only `leave` frees it (into `finished`) |
| B3 | the started run decides the helper's model (the node maps "haiku" to an id) | `input.model`, else `default_model()` |
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
}

/// A started member's run: what the host decided about it, and its end.
#[async_trait]
pub trait MemberRun: Send + Sync {
    fn model(&self) -> Option<String> { None }      // B3
    fn caps(&self) -> Option<MemberCaps> { None }   // B4
    fn deps(&self) -> Option<AgentDeps> { None }    // B5
    async fn end(self: Box<Self>, end: MemberEnd) {} // B6
}

pub struct MemberStart { name, id, parent, depth, input: SpawnInput, model, caps: MemberCaps }
pub struct MemberEnd { ending: Ending, answer: String, spend: Spend }
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
5. the agent is built from the handle: its `deps()` (else the parent's), its `model()`
   (else the call's, else the default), its `caps()` (else `MemberStart::caps`);
6. the turn runs; `on_subagent` sees `Started`, every `Loop` event, `Stopped` as today;
7. it leaves the table (a message for it is refused from here), late messages are taken;
8. `end`, awaited, with how it ended, the answer as the caller gets it, and the spend;
9. the hold is released; the answer goes back (foreground) or is handed back (background,
   still counted in `answers_due` until then).

### Why this shape

- **`end` lives on the handle, not on `SubagentParent`.** The handle is the host's per-member
  state (brain's `GatewayRun`, its record log). On the parent, `end_member(run, ..)` would need
  the parent to keep a map by id (brain's `logs: Mutex<HashMap<..>>`) or downcast an `Any`.
  Taking `self: Box<Self>` makes "end exactly once" a type fact: the hub gives the handle up.
- **The handle overrides `deps`, not single fields.** brain's helper differs in its models
  (its token), its hooks (its log, the pause gate, tagged events) and nothing else of note; a
  host that builds a run elsewhere builds its deps whole. Own deps are used as they are,
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
| `HubHost::start_run(name, model, stop)` | `SubagentParent::start_member(&MemberStart, cancel)` | `MemberStart::{name, input.model, ..}`; `Err(String)` -> `Err(AgentError::Refused(text))` |
| `GatewayRun` | the host's `impl MemberRun` holding it | |
| `run.model` | `MemberRun::model()` | |
| `NoteCaps` from `run.limits` | `MemberRun::caps() -> MemberCaps` | the note and the member's turn caps both read it |
| `HubHost::new_agent(name, run, tools, prompt)` | `MemberRun::deps()` | the hub supplies tools and prompt; drop brain's own Agent construction |
| `HubHost::stop_run(run, reason, answer) -> (cost, calls)` | `MemberRun::end(MemberEnd)` | reason: `Ending` (`Answered` = "done", `Stopped` and `CallerCancelled` = "stopped"); cost: `spend.usd()`; calls: `spend.model_calls` |
| `Event::AgentStart` | emitted from `start_member` (or `on_subagent(Started)`) | prompt, kind, background in `MemberStart::input`, model from its own run |
| `Event::AgentEnd` | emitted from `MemberRun::end` | |
| `Hub::started()`, `tree_limits` | a counter the host bumps in `start_member` | |
| `HubHost::base_tools`, `subagent_tools(base, kind)`, `MAIN_ONLY`, `EXPLORE_TOOLS` | `SubagentParent::subagent_tools(kind)` | haest drops the task tools itself |
| `subagent_types()`, `model_choice` | `SubagentParent::agent_tool_options()` | |
| `HubHost::system_prompt(run, read_only)` | `SubagentParent::system_prompt(model_spec, tools)` | read-only = no `Edit` in `tools`; the model's inputs from `ModelFacts::input_modalities(model_spec)` |
| `HubHost::to_main(text, _)` | `Inbox::submit(text)` | steering main when it runs stays the host's (as `ChatHost::to_main` does) |
| `HubHost::tell` + `Watchers` + `drive` (a helper's tagged events) | `SubagentParent::on_subagent(id, Loop(ev))` | |
| `Table`, `Member`, `Status`, `slug`, `reserved` | `Members` (hub-private) | |
| `Hub::spawn` / `deliver` / `stop` / `stop_all` / `running` | `SubagentHub`'s | |
| `Hub::run_member` (own task, crash = answer) | hub-private `run_member` + `drive` (`catch_unwind`) | |
| `Hub::hand_back` | hub-private `hand_back` | |
| `Hub.caps.depth` offset | `SubagentCaps::max_depth = max_depth - depth` | B15 |
| `Hub::from_person`, `Event::AgentMessage` | NOT YET: G-H1, G-H2 | |
| `to_main(text, message)`, `MainInbox` | NOT YET: G-H3 | `MainInbox` itself stays brain's (it is chat.rs's continuation policy) |
| `Hub::idle`, `Hub::settled`, `pulse` | NOT YET: G-H4 | `answers_due() == 0 && running().is_empty()` polled until then |
| `subagent_note(.., kind)` | `subagent_note` (haest's text) | G-H5: the explore line moves into `system_prompt` |
| `HUBS`, `register`, `message_agent` | stays brain's (its registry by run id) | needs G-H1 |

## Not in this seam (follow-ups)

| Code | Gap | Proposed shape |
|---|---|---|
| G-H1 | the person writing to a member (B11) | `SubagentHub::deliver_as(sender: Sender, to, text)` with `Sender::{Agent(name), Person(label)}`; `deliver` = `Agent` |
| G-H2 | a delivery event (B10) | `SubagentEvent::Message { from, to, text, person }` through `on_subagent` (`SubagentEvent` is not `#[non_exhaustive]`: breaking, 0.3.0) |
| G-H3 | message vs answer for main (B12) | `Inbox::submit_answer(text)`, default `submit` |
| G-H4 | idle signal (B13) | `SubagentHub::idle()` + `settled(timeout)` on a `watch` the hub bumps when a member ends or an answer is handed back |
| G-H5 | the host's own note (B14) | `SubagentParent::subagent_note(&MemberStart) -> Option<String>`, default haest's |

Each is small and independent; none blocks the seam. Durable subagents (`durable-runs.md` R6)
build on `MemberRun::deps` (a member's own sink) and `end` (its record closed).

## Budgets (G3) on the hub

`budgets.md` section S, built on this branch after the seam:

- `Ledger::{reserve, release, available, held}`: holds on the conversation's USD;
  available = cap - spent - held, floored at 0, None with no USD cap. A hold is released
  exactly once (`release` takes it by value).
- The Agent tool's `budget {usd, minutes}`, offered only when `AgentToolOptions::budget_caps`
  is Some; `SubagentHub::tools_for` sets it from its caps when `SubagentCaps::budgets` is on
  (the `subagent_budgets` setting, default off; the `claude` profile leaves it off).
- Each value only tightens (an absent one = the cap); `SpawnInput::budget` carries the
  clamped values to any `AgentDirectory`.
- The time must fit what is left of the caller's turn (`ToolContext::turn_time_left`, filled
  by the loop from its guard). Q6 (refuse or shrink) is decided in ONE function,
  `agents::budget::fit_time`: refuse, with what is left.
- The hub holds the USD (step 2 above) and runs the member under the budget's caps. A child
  with no cap (the cap is 0 and none asked) does not fit under a capped turn or ledger: the
  model is told what is left and names an amount.
