# Budgets: wind-down, subagent budgets, tool costs (plan, opt-in)

Status: plan, 2026-11-16; W and W2 built 2026-11-17. Names follow `main` at `1c7372a` (0.2.0): `caps::{TurnCaps,
SubagentCaps}` hold `Duration`s (minutes only at the settings boundary), `Clock` sits on
`AgentDeps`, the hub's parent is `SubagentParent`, public errors are typed, and every
library message about a cap ends with the host's remedy through `remedy::{Limit, Remedies}`.

Source: protokol agent-core, `usage-budget.ts`, `budget-ledger.ts`, `tool-gate.ts`,
`tool-budget.ts`, `agent-tool.ts`.

Rules for all three (decided 2026-11-16): configurable and opt-in, default off; the
`claude` profile leaves them off. Library messages state the fact and the setting's
plain name, and the host adds its own remedy (the 0.2.0 rule).

## What haest has today

| | Where |
|---|---|
| Conversation ledger: USD + unpriced tokens; posting again does not reset it; the human raises it; saved in the session log; warns the human at 80% | `haest-core` `ledger.rs` |
| Turn caps: USD, tokens, request size, steps, wall time, repeats; nudges the model at 50%, stops at the cap | `caps.rs` `TurnCaps`, `guard.rs` (crate-private) |
| Subagent caps: depth, how many at once, and each one's USD, steps and time | `caps.rs` `SubagentCaps` |
| A tool's own spend, reported after the call: `ToolOutput::cost_usd` (charged to the ledger), `child_cost_usd` (already charged) | `tool.rs` |

## W. Wind-down (agent-core's "soft phase")

When spend reaches a set share of a cap, two things happen once:

1. the Agent tool refuses new subagents (a typed refusal the model reads: "the
   budget is nearly spent: $8.10 of $10 (`max_conversation_usd`); no new subagents");
2. the model gets one notice at the next step boundary: conclude with what it has, start
   no new work. It rides the same path as the guard's nudge (`inject_nudge`).

The caps it watches: the conversation ledger (USD, unpriced tokens) and the turn's USD,
tokens and wall time. Each subagent watches its own caps the same way.

Setting: `wind_down_at`, a share between 0 and 1. 0 = off, the default.

Differs from the guard's nudge: the nudge says "you are burning X, find the exit" at
50% of each turn cap. Wind-down changes behaviour: spawns stop.

## W2. Thresholds: wind-down becomes one action (as built, 2026-11-17)

Built on `wt/wind-down`, on the 0.2.0 names; W is its wind-down action. Code:
`haest_core::threshold`, runnable: `cargo run -p haest --example threshold`.

A threshold = a meter + a mark + an action + when it fires again.

| Part | As built |
|---|---|
| `Meter` | `ContextTokens`, `Steps`, `Turns`, `WallTime`, `Usd(Scope)`, `Tokens(Scope)`, with `Scope::{Turn, Conversation}`. A fixed list, no expression language |
| `Mark` | `Mark::share(0.8)` of the meter's cap, `Mark::at(v)` in the meter's unit, or both (`or_at`: whichever comes first). A share of a meter with no cap never fires |
| `Action` | `Remind` (the model), `Warn` (the human), `WindDown` (no new subagents), `Compact`, `Stop` |
| `Rearm` | `Never`, `NextTurn`, `BelowMark`, and `EveryCheck` (below) |

`Threshold` is a trait: meter, mark, action, rearm, the text it says, and an optional
`closing` said once after every reminder that shares it. A threshold is checked where its
meter is read, a `Site`. A site holds a `Thresholds<S>` list (`Default` = the site's
defaults) and a `Watch<S>` that remembers which fired. A threshold a site cannot check is
refused with a typed `NotServed` when it is added, never ignored.

| Site | Set on | Checks | Reads | Can |
|---|---|---|---|---|
| `TurnSite` | `SharedDeps::thresholds` | at every step boundary; Stop before every model call | context tokens (cap: the new `TurnCaps::context_window`), steps, wall time, the turn's and the conversation's USD and tokens | remind, warn, wind down, stop |
| `LedgerSite` | `Ledger::set_thresholds` | at every charge | the conversation's USD and unpriced tokens | warn |
| `AgentTurnSite` (haest-tools) | `AgentTurnGate::with_thresholds` | at every turn no human started | turns | remind, warn |
| compaction | `CompactionOptions` is the threshold, `needs_compaction` its check | before every model call, in the host's hook | context tokens | compact |

The five defaults keep today's numbers and behaviour. Their existing tests pass unchanged:

| Default | Meter | Mark | Action | Rearm | Pinned by |
|---|---|---|---|---|---|
| the guard's nudge | steps, wall time, the turn's USD and tokens | `NUDGE_SHARE` (0.5) | remind | next turn | `guard/tests.rs` |
| the agent-turn gate's nudge | turns no human started | 0.5 | remind | next turn (each such turn) | `turn_gate.rs` tests, its doctest |
| the ledger's warning | the conversation's USD and unpriced tokens | `WARN_SHARE` (0.8) | warn | below mark; `set_cap` and a restore settle it | `ledger` tests, its doctests |
| compaction | context tokens | `COMPACT_SHARE` (0.8) of the window, or `compact_at` when lower | compact | every check | `compaction` tests, haest-runtime's compaction tests |
| wind-down (off) | the conversation's USD and unpriced tokens, the turn's USD, tokens and wall time | `wind_down_at` | wind down | next turn | `wind_down/tests.rs`, haest-tools `agents/tests/wind_down.rs`, haest-config `tests/wind_down.rs` |

`wind_down_at` stays the named setting: haest-runtime adds `Thresholds::default().wind_down(at)`,
which is nothing when it is 0. The tools' live question (`ToolContext::wind_down`,
`WindDownWatch`) asks `Thresholds::past`, which keeps no state: a spawn is refused for as
long as the run is past the mark, and the notice is said once a turn.

Where the table this replaced was wrong, checked against the code:

- **Compaction** fires at every check past the mark, not "once compaction drops it":
  `needs_compaction` runs before every call, and a check that cut nothing tries again at the
  next. Hence a fourth rearm, `EveryCheck`.
- **The ledger's warning** re-arms below the mark (a raised cap), not "never": `Watch::settle`
  is how `set_cap` and a restored ledger take the spend so far as already heard.
- **The gate** counts turns no human started, not a subagent's turns, and nudges at every
  such turn from half its cap.
- **Repeats** are not a threshold: the repeat check keeps one count per distinct tool call,
  not one meter. It stays in the guard, and its nudge still ends with the same way out.

Not thresholds: the caps themselves (a turn stops AT a cap; a Stop threshold stops it
earlier), the request cap (one request, not a meter), the repeat check.

Decided while building:

- A turn's warning goes to `AgentHooks::on_warning` (default: nothing), the way the ledger's
  goes to `LedgerHooks::warn` and the gate's to its notice. A new `LoopEvent` would have broken
  every exhaustive match on it, an existing test's included. haest-runtime sends it to `Ui::warn`.
- The reminders due at one boundary reach the model as ONE `[haest guard]` message: each text,
  then each distinct closing once. For the defaults it is the old nudge, byte for byte.
- Each agent keeps its own `Watch<TurnSite>` while it lives. A host that changes
  `SharedDeps::thresholds` between turns keeps the fired state of the thresholds it kept
  (the same `Arc`, `Watch::follow`).
- A durable run's step ends at the one boundary, `after_tools`, whose first act is
  `at_boundary` (the thresholds); a resumed step goes through it too. The turn's record
  (`TurnState::nudged`) lists the thresholds that fired, `threshold <n>: <action> on
  <meter>`, beside the nudged repeats, so a resumed turn neither reminds twice nor says it
  is winding down twice.
- On the Agent tool, wind-down is asked before a budget: a turn that winds down starts no
  subagent, whatever budget it asks for.
- Compact is no turn threshold: the loop never compacts on its own, so `TurnSite` refuses it.
- Naming: the mechanism is "thresholds" and remind is `Action::Remind`, as decided. "Fires
  again" is `Rearm` in the code: a short noun the four kinds read well after
  (`Rearm::BelowMark`).

## S. Subagent budgets (agent-core's `clampUsd` + spawn reservation)

With the setting on, the Agent tool takes an optional `budget`:

```json
{ "budget": { "usd": 0.25, "minutes": 3 } }
```

- **Only tightens.** Each value is clamped to the operator's `SubagentCaps`: a model can
  ask for less than the cap, never more. An absent value = the cap.
- **Must fit.** The child's USD is held against the conversation ledger while it runs
  (available = cap - spent - held). A spawn that does not fit is refused with what is
  left, so the model can ask for less. When the child ends, the hold is released and
  its real spend stays (it charged the ledger itself, call by call).
- **Time must fit.** The child's minutes must fit what is left of the parent turn's
  wall time (`TurnCaps::max_turn_time`); otherwise refused with what is left. Minutes
  exist only in the tool's parameter; inside it is a `Duration`.
- Concurrent children are why the hold matters. Each subagent checks the shared ledger
  only before its own model calls, so 4 running at once can each pass the check and
  overshoot together. Holding each child's cap up front closes that.

The parameter appears in the tool's schema only when the setting is on, so the `claude`
profile's Agent tool keeps Claude Code's schema.

Setting: `subagent_budgets`, a bool, default false.

Brain: `SpawnInput` gains the clamped budget. Brain's own hub (`AgentDirectory`) reads it
and applies it to its node runs; haest's default hub (`SubagentHub`) applies it itself.

## T. Tool costs: initial + final

A tool declares an optional INITIAL cost from its input (an image model: images x price),
and reports an optional FINAL cost when it ends.

| How the call ends | Charged |
|---|---|
| the tool reported a final cost (success or failure) | the final |
| it failed and reported none | the initial |
| bad input (`ToolError::Input`, nothing was called) | nothing |
| it succeeded and reported none | the initial |

The initial is a HOLD taken before the call runs, checked against what is left (the same
hold as S), so parallel paid calls cannot overshoot together.

### Can it wait? Yes.

- The final already exists: `ToolOutput::cost_usd`.
- The initial is one new DEFAULT method on `Tool` (`fn cost_hold(&self, input) -> Option<Hold>`,
  default None). Every existing tool, and every host's tool, compiles and behaves as
  today. Adding it later breaks nothing.
- Today it would buy little: only read-only tools run in parallel (`tool_batch.rs`), and
  the paid ones (GenerateImage, Agent) are not read-only, so they run one at a time and
  the check before the next model call bounds an overshoot to one call.
- The hold mechanism lands with S (subagents are the concurrent spenders today). T reuses
  it when a tool needs it, for example an MCP tool that is both paid and read-only.

## Order

1. S's ledger holds (reserve, release, available).
2. S: the `budget` parameter, the clamp, the spawn checks.
3. W: wind-down. Built, as W2's thresholds.
4. T, when a parallel paid tool exists.

Each with tests first (behaviour, not wording), settings rows with examples, and
`docs/settings.md` regenerated.

## Open questions

Numbered after `durable-runs.md`'s Q1-Q4, so a code means one question across both plans.

- **Q5.** Wind-down share: one setting for every cap, or one per cap? Built as one;
  `WindDown::share_for` is the one place a per-cap share would go.
- **Q6.** A subagent whose minutes do not fit: refuse (agent-core) or shrink it to fit?
