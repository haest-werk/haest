# Durable runs (plan, opt-in)

Status: P1 and P2 built (2026-11-16, `haest_core::run`, `Agent::run_durable`); P3-P5
are plans. Type and file names follow `main` at
`1c7372a` (0.2.0): `caps::TurnCaps` (`max_turn_time: Duration`), `SubagentParent`
(`agents/parent.rs`), the hub's member run in `agents/hub/member_run.rs`, `ModelFacts` and
`Clock` on `AgentDeps`.

### Built in P1 + P2, and where it differs from the design below

- `RunStore` (R7) with the memory provider only, on `SharedDeps::run_store` (a subagent
  shares its parent's store, which P4's child keys need); `Agent::run_durable(key, input, sink)`.
- The `turn` record keeps `elapsedMs`, not `startedMs`: a resumed turn's time cap counts on
  from the time it had run, so the time the run was stopped does not count. It also keeps
  `nudged` (no second nudge after a resume) and `requestTokens` (the room a tool result has).
- `turn` is written after every model call and every tool result, so a crash mid-step loses
  no spend or repeat count.
- `run` keeps `base`, the history's length at the start: a resume checks the agent's history
  against it and cuts back a history that already holds the run's messages.
- A run that ends with an error or an abort writes no `run_end`: a second start resumes it
  (a provider error is retried, not served forever).
- Which pending calls "may have run": only the first batch of the last answer's calls without
  a result. A later batch with side effects cannot have started (results land in call order,
  and the loop flushes before every batch with side effects), so it runs once.
- The guard's repeat keys and the effect keys use a hash that is the same in every build
  (FNV-1a), so a resume after a deploy built with another toolchain still matches.
- No `durable` setting yet: a memory store is useless across processes, so the row comes with P3.

## Goal

A run that a crash, a deploy or a kill stops part way continues where it stopped,
without paying again for model calls that finished or repeating tools that finished.
A run started twice under the same run key returns the first run's result. Opt-in: a
host that hands haest no store gets exactly the haest of today.

Source of the idea: protokol's agent-core execution journal
(`~/projects/protokol/PLAN.md`, `packages/agent-core/src/journal-*.ts`,
`workflow-snapshot.ts`).

## What haest already has

agent-core had to memoise every model call by a hash of its input, because its loop
lives inside an opaque `Generate` brain that never shows it a tool call (protokol
`PLAN.md` section 1b). haest owns its loop, and the loop's state is the message history:

- every message goes through `AgentHooks::on_message` into a `SessionSink`, in order
  (`agent/turn.rs`, `append`);
- `SessionSink::flush` is a barrier: it resolves once every record appended so far
  has landed (`session/file.rs`);
- `session::replay` rebuilds the history and answers a tool call that has no result as
  interrupted (`session.rs`, `answer_interrupted`);
- the ledger and the task list are absolute records, the last one wins;
- the TS haest skips record kinds it does not know (`session-store.ts`:
  `REPLAY[r.type]?.(...)`), and so does the Rust one (`session.rs`, `KINDS`), so new
  record kinds stay compatible both ways (D3).

So a model call needs no memo: a model call that finished IS an assistant message in
the log. What is missing:

| Code | Gap | Where today |
|---|---|---|
| J1 | A turn can be restarted, never continued. Replay answers the dangling calls as interrupted and the host starts a NEW turn with new input | `session.rs` `answer_interrupted` |
| J2 | Turn state is not in the log: turn spend, step count, start time, the guard's repeat counts. A resumed turn would start with fresh turn caps | `agent/turn.rs` `Turn` |
| J3 | No intent before effect. `on_message` only queues the record; nothing waits for the assistant message (its tool calls ARE the intents) to land before a tool with side effects runs | `session/file.rs` write queue |
| J4 | A tool cannot say whether running it again is safe; `read_only()` is the only signal | `tool.rs` |
| J5 | Subagents keep no log ("neither persists its messages nor compacts", `agents/parent.rs`). A crash runs, and pays for, every subagent again from the start | `haest-tools` `agents/` |
| J6 | No run key: nothing maps "this job" to "its result" | -- |
| J7 | Background jobs (Bash `run_in_background`, background agents) die with the process | `haest-tools` `jobs.rs` |

## Design

### R1. Run key

`RunKey`, chosen by the host. Starting a durable run with a key:

| The store has | haest does |
|---|---|
| a finished record for the key | returns that result: no model call, no tool, no spend |
| a started record with no end | resumes it (R5) |
| nothing | starts it |
| a record whose input differs | refuses (input hash mismatch) |

Child keys derive from the parent, so a replay finds them again:
`<parent>/<tool_call_id>` for a subagent, `<parent>/<step name>` for a flow step (R8).
This is the shape every durable-execution system uses: Restate's `Idempotency-Key`,
DBOS's workflow id, Temporal's workflow id.

### R2. Records

New session record kinds, added beside the existing six. camelCase, last one wins
where absolute:

| Kind | Fields | Written |
|---|---|---|
| `run` | `key`, `inputHash` | when a durable run starts |
| `turn` | `usage`, `steps`, `startedMs`, `repeats` (absolute) | at every step boundary |
| `run_end` | `key`, `stopReason`, `text`, `usage` | when the run ends; a second start returns it |

The ledger already writes absolute snapshots, so spend needs no new kind.

### R3. Intent before effect

Before a batch that holds any tool that is not read-only, the loop awaits a new
`AgentHooks` method (default: returns at once) that a durable host maps to
`SessionSink::flush`. Read-only batches do not wait. Cost: one flush per step that has
side effects. The `durable-agent` crate fences each tool step the same way (a durable
`step_started` before the effect).

### R4. What a tool does on resume

A new default method on `Tool`, so existing tools compile unchanged:

```rust
fn on_resume(&self) -> OnResume {
    if self.read_only() { OnResume::Rerun } else { OnResume::Report }
}
```

(Named `OnResume`: `haest_runtime::Resume` already names which session a build resumes.)

| `OnResume` | Meaning |
|---|---|
| `Rerun` | running it again is safe (reads; a Write of the same bytes) |
| `Report` | answer the call "interrupted: it may or may not have run; check before retrying" (today's behaviour, now chosen per tool) |
| `Lookup` | the tool can find out: it gets the call's effect key and asks its backend |

The effect key is a stable hash of (run key, tool call id), offered on `ToolContext`.
A tool passes it to an external API as that API's idempotency key (a payment, an image
job). Then a rerun of an effectful call is safe too. zeph-durable and the Execution
Ledger write-up build keys this way (run id + step id).

### R5. Resuming a turn

`Agent::resume` (or `run_durable(key, input)` choosing between start and resume):

1. replay the run's records, keeping the last assistant message's calls unanswered;
2. answer each dangling call by its tool's `Resume`;
3. restore turn state from the last `turn` record, so the turn caps continue;
4. enter the normal step loop.

The loop needs no determinism rule (unlike Temporal workflow code): the history is
data, and the model is asked again only for steps that never finished.

### R6. Durable subagents

When the parent run is durable, the hub gives each subagent its own sink under
`<parent key>/<tool_call_id>`. A parent that resumes with a dangling Agent call resumes
that child from its log instead of spawning a new one. The child's spend is already in
the shared ledger's snapshots. A background subagent resumes the same way; its answer
goes to the inbox as today.

### R7. Stores (providers)

One trait. Each provider is its own opt-in piece; none is on by default.

```rust
#[async_trait]
pub trait RunStore: Send + Sync {
    /// Where the run's records go (the existing seam).
    fn sink(&self, key: &RunKey) -> Arc<dyn SessionSink>;
    /// The run's records so far; None for a key never started.
    async fn load(&self, key: &RunKey) -> io::Result<Option<ReplayedRun>>;
}
```

| Provider | Lives in | Notes |
|---|---|---|
| memory | `haest-core` | tests, and hosts that only need to survive an aborted turn |
| disk (JSONL) | `haest-core`, over a handed `Fs` | `JsonlSessionFile` already is this, one file per key |
| SQLite | new crate `haest-store-sqlite` (rusqlite) | brain links rusqlite 0.40 already (`brain/node/Cargo.toml`); one table of records keyed by run key |
| host functions | `FnStore` in `haest-core` | the host hands closures: brain's node API, a remote service, a Cloudflare Durable Object |

Q1 asks what "function call" meant: these closures, or the step-engine shape below.

The step-engine shape: haest hands each unit of work to the host as
`step(key, work) -> result`, and the host's engine (Restate `ctx.run`, Cloudflare
Workflows `step.do`, DBOS steps, Temporal activities) records and replays it. That
lets a haest run live inside one of those engines. It is a larger change (every model
call and tool call becomes a step), so it comes after the records-based stores, as an
adapter over the same records.

### R8. Parallel and pipeline

For a host's own Rust code, built on run keys. A new crate `haest-flow`, behind a
feature:

- `parallel(steps)`: a barrier. Every step reads the same state; their results join in
  input order. A step that fails gives None and changes nothing.
- `pipeline(items, stages)`: no barrier between stages; item A can be in stage 3 while
  item B is in stage 1.
- A leaf is a subagent run, one model call or plain host code, each under
  `<run>/<step name>`. A finished step is served from the store on resume; an
  unfinished one runs again.

The model is agent-core's `workflow-combinators.ts` and `workflow-snapshot.ts`.
One deliberate change: step names are explicit. agent-core numbers unnamed nodes
(`kind#N`), and its own docs say editing the body between snapshot and resume shifts
those ids and turns finished steps into misses.

## Opt-in and settings

- The library: nothing is durable unless the host hands a `RunStore` (D15, D16:
  `RuntimeInputs` gains an optional store).
- haest-config rows, all default off: `durable = off | memory | disk | sqlite`; the CLI
  gets `--run-key`.
- The `claude` profile leaves it off.

## Guarantees and limits

- Side effects run at least once. Exactly once only where a tool passes its effect key
  to an API that honours it (protokol `PLAN.md` rule 8 holds here too).
- A model call in flight at the crash is paid again: its answer never landed. At most
  one call per crashed run.
- The assistant message and the ledger snapshot after it are two queued records. A
  crash between them under-counts the ledger by one call. Writing the ledger record
  first, or both in one write, closes it.
- Background Bash jobs are not resumed; they are answered as interrupted.

## Phases

| Phase | Work | Done means |
|---|---|---|
| P1 | `RunKey`, `run` + `run_end` records, the finished-run shortcut (R1, R2) | a second start with the same key returns the stored result and the scripted provider sees zero requests |
| P2 | `turn` record, resume of a turn, the flush before effects, `Resume` per tool with effect keys (R2-R5) | a scripted run killed after tool 2 of 3 resumes: tool 3 runs once, read-only calls rerun, the others follow their `Resume`, turn caps continue from the restored spend |
| P3 | Stores: memory, JSONL, SQLite crate, `FnStore` (R7) | the P2 test passes on each store |
| P4 | Durable subagents (R6) | a crash inside a subagent resumes the child, not a fresh one |
| P5 | `haest-flow`: `parallel` + `pipeline` (R8) | a flow killed mid-pipeline resumes and runs only the unfinished steps |
| later | the step-engine adapter (R7); flow from the Code tool | -- |

## Open questions

- **Q1.** "Function call" store: host closures (R7 table), or the step-engine shape?
- **Q2.** In brain, is the node's run id the run key?
- **Q3.** Should a resumed turn tell the model it was resumed (one note), or stay silent?
- **Q4.** How long is a finished run's result kept for the shortcut (forever, a TTL, the
  host decides)?

## Sources

- protokol agent-core: `PLAN.md` (execution journal), `journal-reduce.ts`,
  `journal-resume.ts`, `gated-generate.ts`, `workflow-snapshot.ts`, `workflow-combinators.ts`
- Restate, durable steps and idempotency keys: <https://hda.daz.is/integrations/background-jobs-restate/>
- durare, a DBOS-compatible durable-execution SDK for Rust: <https://github.com/sharmis-tp/durare>
- zeph-durable (step ids, intent/result, `IdempotencyKey::derive`): <https://docs.rs/zeph-durable/latest/zeph_durable/>
- durable-agent (journal before effect for tool steps): <https://docs.rs/durable-agent/latest/durable_agent/>
- Execution Ledger (idempotency key from run id + step id): <https://luke.geek.nz/azure/execution-ledger/>
- Temporal vs Restate vs DBOS for agents (2026): <https://ecn-apps.com/pages/articles/durable-ai-agents-temporal-vs-restate-vs-dbos-2026.html>
