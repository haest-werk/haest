# Budgets: wind-down, subagent budgets, tool costs (plan, opt-in)

Status: plan, 2026-11-16. Names follow `main` at `1c7372a` (0.2.0): `caps::{TurnCaps,
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

## W2. Thresholds: wind-down becomes one action (decided direction, 2026-11-17)

haest already has five hard-coded thresholds of one shape, a meter crossing a mark fires
an action:

| Today | Meter | Mark | Action | Fires again |
|---|---|---|---|---|
| the guard's nudge | each turn cap: USD, tokens, steps, time, repeats | 50% (`NUDGE_SHARE`) | tell the model | next turn |
| the agent-turn gate | a subagent's turns | 50% | tell the model | -- |
| the ledger's warning | conversation USD, unpriced tokens | 80% (`WARN_SHARE`) | tell the human | never (saved) |
| auto-compaction | context tokens | 80% of the window, or `compact_at` | compact | once compaction drops it |
| wind-down (built, unmerged) | the conversation and turn caps | `wind_down_at` | refuse new subagents + tell the model | next turn |

The cap itself is the same shape at 100%: stop.

A threshold = a meter + a mark + an action + when it fires again.

- **Meters:** context tokens, steps, turns, wall time, USD, tokens; turn scope or
  conversation scope. A fixed list, no expression language.
- **Mark:** a share of the meter's cap, or an absolute value.
- **Actions:** remind the model, warn the human, wind down (refuse new subagents), compact, stop.
- **Fires again:** never, next turn, or once the meter falls back under the mark.

The five become default thresholds with today's numbers, so nothing changes until a host
changes them; the existing tests pin that. `wind_down_at` stays as the named shortcut for
the wind-down threshold. A threshold is a trait, so a host or a mod adds its own.

Built after Wave C of the 0.2.0 review lands: the guard, the ledger's warning and the
compaction threshold are what it is moving. `wt/wind-down` is not merged; it is rebuilt on
this, and its tests stay as the wind-down threshold's acceptance.

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
3. W: wind-down.
4. T, when a parallel paid tool exists.

Each with tests first (behaviour, not wording), settings rows with examples, and
`docs/settings.md` regenerated.

## Open questions

Numbered after `durable-runs.md`'s Q1-Q4, so a code means one question across both plans.

- **Q5.** Wind-down share: one setting for every cap, or one per cap?
- **Q6.** A subagent whose minutes do not fit: refuse (agent-core) or shrink it to fit?
