# Backends and transports (plan, opt-in)

Status: plan, 2026-11-16. Nothing here is built. Names follow `main` at `eb1a493`; the 0.2.0
clean-code pass renames some of them (`AgentHooks`, the subagent hub), so update the names
when this is built.

## Goal

A haest conversation another program can watch and drive while it runs: from a second
terminal, from a parent process over stdio, from a broker that shows a fleet. Each piece is
its own crate or feature, off by default, and a host composes only the ones it wants. A host
that takes none of them gets exactly the haest of today.

Source: protokol's `agent-core` seam (`AgentBackend`, `ControlTransport`, the event hub) and
`agent-protocol`, `agent-host`, `agent-broker`, `agent-ws-backend`, `agentctl`
(`~/projects/protokol/packages/`, at `e8d0baa`).

Who needs this: brain does NOT. It links haest without `runtime` (`brain/node/Cargo.toml:61`),
drives turns with its own sink (`run/drive.rs:90-151`), numbers its own run events for its live
socket (`web/live.rs:88`, `219-220`) and runs helpers through its own `AgentDirectory`
(`run/agents.rs:342`). The consumers are the `haest` CLI (peek and steer, run as a child) and
remote-claude (a haest child behind its broker). See Q8.

Decided 2026-11-16: build it (P1-P5), even though brain does not consume it; brain is the
first consumer for the rest of the agent-core port (Q8). Q7 and Q9-Q11 go with this plan's
recommendations unless overruled.

## 1. What exists

### protokol, read and checked

| Piece | Where | What it is | State |
|---|---|---|---|
| `AgentBackend` | `agent-core/src/agent-backend.ts:37-47`, `localBackend` 51-61 | one handle to a live session: `meta`, `control`, `events`, `turn()`, `close()` | thin adapter over `buildHarness` |
| `ControlSurface` | `agent-session.ts:44-52` | snapshot, transcript, tasks, subscribe, inject, interrupt | keeps its own transcript array beside the model history |
| `ControlTransport` | `control-transport.ts:40-43`, `attachTransports` 53-78 | `attach(ctx) -> detach`; every transport attaches on its own, one failing does not stop the others | wired via `HarnessConfig.transports` (`harness-config.ts:82`) |
| Event hub | `agent-events.ts:84` (envelope `v, seq, ts, agentPath`), 87-96 (`gap`), 120-152 | sync fan-out, bounded ring (1000), `replayFrom(afterSeq)` with an explicit gap flag; `EVENT_SCHEMA_V = 3` (line 20), new types are additive | used by agentctl and the broker link |
| `agent-protocol` | `envelope.ts:12-157`, `validate.ts`, `framer.ts` | 14 frame types on `t`, `v/id/sid/seq/ts`, version checked before shape | types + validator + framer only |
| `agent-host` | `process-host.ts:41-92`, `ws-server.ts:42-78`, `replay-buffer.ts:16-31`, `ndjson.ts` | spawn a child, NDJSON over stdio; a WS server; a per-session ring (500) | the pieces are not wired to each other anywhere: only their own tests and ws-backend's test use them; no child adaptor exists |
| `agent-broker` | `broker.ts:58-230`, `ws-transport.ts:36-105`, `agent-link.ts:30-83`, `auth.ts:54-82`, `send-queue.ts`, `backoff.ts` | agents dial out; token in the first frame; read-only vs command viewers; bounded queue that drops oldest and sends a gap; full-jitter backoff; stop retrying on close 4401/4408 | works; its own envelope (`kind`, `BROKER_PROTOCOL_V = 1`, `protocol.ts:15`) |
| `agent-ws-backend` | `ws-backend.ts:26-80`, `remote-session.ts` | controller-side `AgentBackend` over agent-protocol | MVP: `tasks()` empty, usage zero (`remote-session.ts:56,63`) |
| `agentctl` | `control-server.ts:29-139`, `unix-transport.ts:9-16`, `agentctl.ts` | Unix socket + discovery file `<dir>/<id>.json` (0700 dir, 0600 socket); `{rid, cmd, args}` lines; verbs ping, snapshot, transcript, tasks, inject, interrupt, subscribe, events | works |

The three claims in the brief hold. (1) The `agent` tool spawns through `spawnSubAgent`, whose
`runner` is the local `gatedGenerate` (`agent-tool.ts:89`, `agent-runtime.ts:75-78`);
`AgentBackend` is used only by `localBackend` and `createWsBackend`. (2) wsBackend keeps one
`pendingTurn` and has no reconnect (`ws-backend.ts:7-9`, 33). (3) `tool_request`,
`mcp_request` and `sync_*` are typed and validated, and no code handles them.

protokol has three envelopes for one job: agent-protocol frames (`t`), broker messages
(`kind`, carrying `AgentEvent` v3) and agentctl's unversioned `{rid, cmd}`. haest gets one.

### Found while reading: do not port these

| Code | Where | What |
|---|---|---|
| B1 | `agent-link.ts:46-51`, `57-59`, `73-77` | Bug. The resume point is the last seq QUEUED locally; a frame handed to a socket that was already closing counts as sent. The broker's `registered.ackSeq` arrives after the replay ran and can only raise the point (`Math.max`). Those frames are lost with no gap signal |
| B2 | `ws-backend.ts:68-73` | Bug. A second `turn()` overwrites `pendingTurn`, so the first never resolves; no `onClose`, so a dropped socket leaves a turn pending forever |
| B3 | `broker.ts:82-85`, `fleet.ts:35-43`, `auth.ts:75` | Bug (token-list auth). Any valid agent token registers ANY `sessionId` and takes over that agent's fleet row and its command routing |
| B4 | `ws-server.ts:48-51`, `ws-backend.ts:82-87` | Design. The shared secret rides in the URL query, which proxies and access logs keep. Use the first frame, as the broker does |
| B5 | `process-host.ts:48` | Design. The child gets the parent's whole env plus extras: against D15/D17 |

Also different by design: agent-core consumes injected text only at the END of a turn, up to 4
rounds (`agent-runtime.ts:25`, `152-180`). haest steers at the next step boundary
(`haest-core/src/agent.rs:105-113`), which is better; keep haest's.

### haest today

| Piece | Where | Note |
|---|---|---|
| Loop events | `haest-core/src/events.rs:41-113` | `LoopEvent`, stable serde shape, `Serialize` only; `EventSink` is called inline: a slow sink slows the turn (147-153) |
| Steer / stop | `agent.rs:93-158` | `AgentControl`: `steer`, `abort`, `running`, `take_queued`; cheap to clone |
| The turn loop | `haest-runtime/src/lib.rs:122-167` (doc example), `haest-cli/src/run.rs:82-151` | the host writes it: `select!` the turn against the inbox, steer arrivals, then run what waits through the gate |
| Inbox + gate | `haest-runtime/src/inbox.rs:76-151` | human vs agent origin (`human`, `agent_turn`): the ping-pong cap counts agent turns only |
| Between-turn changes | `runtime.rs:88-133` | `set_model`, `set_effort`, `clear`: agent, host and session file in step |
| Approvals | `haest-mods/src/gate.rs:23-46` | `Prompter::prompt -> Answer {Allow, AllowAlways, Deny}`; unanswered asks deny after 10 min (50) |
| Subagents | `haest-tools/src/agents.rs:19-23`, `181-196`; `agents/host.rs:16-38`, `68-69` | `AgentDirectory` is the plug point for a subagent that runs elsewhere; `SubagentEvent` is not `Serialize` |
| `--json` | `haest-cli/src/render.rs:94-100`, `165-184` | output only: `LoopEvent` lines plus `notice` and `ask` lines; no seq, no version, no input side |

### Three wire vocabularies for the same session

| Concern | agent-protocol | TS haest host (claudewerk) | haest now |
|---|---|---|---|
| handshake | `hello {backend, capabilities}` | `agent_host_boot` (`host/link.ts:20-38`) | none |
| input | `turn {text, source}` | `input`, `channel_deliver` with human vs agent origin (`host/inbound.ts:83-116`) | CLI stdin, `-p` |
| control | interrupt, poke, set_model, set_effort, set_permission_mode, close | interrupt, clear, quit, set_model, set_effort, set_permission_mode (`inbound.ts:37-58`) | `AgentControl`, `Runtime` setters |
| approvals | `tool_request` / `tool_response {allow, deny}` (no "always") | `permission_request/response` + `permission_rule` | `Prompter` |
| stream | `delta {text, channel}` | `stream_delta` in Anthropic SSE shape (`translate.ts:63-76`) | `text_delta`, `thinking_delta` |
| messages | `transcript {role, content: unknown}` | CC-shaped transcript entries (`translate.ts`) | `user`, `assistant`, `tool_result` with typed messages |
| turn end | `result {stopReason, usage}` | `result_text`, `turn_cost`, `turn_duration` | `turn_end {stopReason, usage, durationMs}` |
| order, catch-up | `seq` per sid; `sync_*` unhandled | the bridge transport's | none |

haest's own events are the richest and already the session file's types. Q7 asks which
vocabulary is native; the plan assumes haest's own, with the others as adaptors outside haest.

## 2. Design

### Rules it keeps

- D9: every piece off until asked for. No new facade feature in `default`
  (`crates/haest/Cargo.toml:18-41`). A built-in transport still does nothing until the host
  attaches it, and an attached one is watch-only unless the host says command.
- D15: the library reads nothing. A socket dir, a broker URL and a token are values the host
  hands in (no `AGENT_CTL_DIR`, `USER` or tmpdir lookup as in `control-paths.ts:8`). A child
  process gets only the env it was handed (B5).
- D16: transports are not `RuntimeInputs`. They attach to a running session after `build`.
- Transports are dumb pipes: frames in, frames out, the access check. Every behaviour (queueing,
  steering, approvals, catch-up) lives once, in the session.
- The host decides who a transport speaks for: its `Access` (watch | command) and the `Origin`
  of its input (human | agent, for the turn gate). A frame cannot raise either.

### Pieces

| Crate | Facade feature | Builds on | What |
|---|---|---|---|
| `haest-wire` | `wire` | serde only | the envelope, inbound frames, framer (seq), replay ring with gap, NDJSON line codec, version gate. No I/O, no tokio |
| `haest-session` | `session` | haest-core, haest-wire, tokio sync | the session driver and its `SessionHandle`; the `Backend` trait; `Driven` (what the driver drives); an approval `Prompter` that asks over the session (sub-feature `approvals`, needs haest-mods) |
| `haest-runtime` | (with `session`) | haest-session | `impl Driven for Runtime`: mods' `rewrite_prompt`, the inbox gate, model/effort/clear, flush, the shutdown in `lib.rs:158-166` |
| `haest-transport` | `transport-stdio`, `transport-unix`, `transport-broker` | haest-session | one feature per transport; the broker one alone pulls a WebSocket client (tokio-tungstenite on rustls) |
| `haest-cli` | -- | all of the above | `--stdio`, the control socket, `haest ctl`, the broker link; each switched on by a setting |

What composes with what: `haest-wire` stands alone (a non-Rust consumer can follow its docs; a
host that only wants sequenced `--json` stops there). The session needs the wire. Every
transport needs a session handle and nothing else, and several attach at once, each failing on
its own (`attachTransports`' rule). The terminal renderer becomes one more subscriber.

### The seams

- `Backend` (the `AgentBackend` seam): `meta`, `submit(input)`, `control(verb)`,
  `subscribe(after_seq)` (a stream of envelopes, with gap frames), `snapshot`, `history`,
  `close`; `turn(input)` = submit and wait for that turn's `turn_end`. `SessionHandle` is the
  local implementation. Transports take `Arc<dyn Backend>`, so a transport can also expose a
  backend that is itself remote (the agent-host relay) with no new code.
- `Driven`: what the driver needs from the thing it drives. The driver owns the `Agent` on its
  own task, runs one turn at a time, steers input that arrives mid-turn (`AgentControl::steer`),
  queues the rest as the next turn through the gate, and applies model/effort/clear between
  turns. A bare `Agent` gets a plain impl; `Runtime` gets the full one.
- `Transport`: `attach(backend, access, origin) -> Detach`.

### The envelope

One JSON object per line or WS frame. Outbound: `LoopEvent` flattened into
`{v, sid, seq, ts, path, type, ...}`, so a `--json` reader keeps every field it reads today.
`path` is `["main"]` or `["main", "<subagent id>"]` (from `SubagentEvent`, which then derives
`Serialize`). Other outbound types: `hello`, `notice`, `ask`, `approval_ask`,
`approval_withdrawn`, `status`, `gap`, `reply {id, ok, result | error}`. Inbound: `turn`,
`control {verb}`, `approval_answer`, `snapshot`, `history`, `events {after, follow}`, each with
an `id` the reply echoes. Version rule from `agent-events.ts:12-19`: a new type is additive, a
changed shape bumps `v`; inbound with another `v` is refused before its shape is read.

The ring (default 1000 frames, a setting) keeps the running turn whole, and drops a model
call's deltas once its `assistant` frame enters (that frame holds their text, so it is not a
gap). Delivery to subscribers is a bounded broadcast: a slow one gets ONE `gap` frame for what
it missed, and the turn never waits on it. On a gap a consumer asks `history` (the messages,
with the seq they are current to) and follows from there. Surviving a crash is the durable-runs
plan's job (`docs/plans/durable-runs.md`), not the ring's.

### How a host composes

- The CLI: `build` the runtime, start a session over it, subscribe the terminal; then, by
  settings: `--stdio` attaches stdio (command, human); `control_socket = watch | command`
  attaches the unix transport in `control_dir`; `broker_url` attaches the broker link with the
  token from its keys.
- A host that embeds haest and wants only a parent process to drive it: wire + session +
  `transport-stdio`.
- brain: nothing changes; it may take `haest-wire` alone if it ever wants the envelope.

### Settings (haest-config registry rows, each with an example)

| Setting | Default | Note |
|---|---|---|
| `stdio` (`--stdio`) | off | frames in on stdin, out on stdout; no REPL; runs until stdin closes or a `close` frame |
| `control_socket` | `off` | `watch` or `command` |
| `control_dir` | `<data_dir>/control` | the loader's default; the library is handed the path |
| `broker_url` | none | `wss://...`; the token comes through the `KeySource` |
| `broker_access` | `watch` | what commands from the broker may do |
| `event_ring` | 1000 | frames kept for catch-up (finished turns) |

## 3. Phases

| Phase | Work | Done means |
|---|---|---|
| P1 | `haest-wire`; `--json` lines carry the envelope | every `--json` line has `v, sid, seq, ts, path` and keeps its old fields; ring, gap, framer, line codec and version gate tested (port `replay-buffer.test.ts`, the replay cases of `agent-events.test.ts`, `ndjson.test.ts`, `framer.test.ts`, `validate.test.ts`); docs.rs clean |
| P2 | `haest-session`: driver, `SessionHandle`, `Backend`, `Driven` for `Agent` and `Runtime`; the CLI's turn loop (`run.rs:82-151`) moves into it | the CLI's tests pass unchanged; new tests: input mid-turn lands at the next step, input while busy runs as the next turn through the gate, a lagging subscriber gets one gap, two subscribers see the same seqs, `close` stops subagents, kills jobs, closes MCP and flushes |
| P3 | `transport-stdio` + approvals over the session; `haest --stdio` | a test drives two turns through pipes, interrupts one mid-tool, answers asks with allow, always and deny, sees a withdrawn ask after an interrupt, gets a refusal for a wrong `v`; run once live against a real model. Approvals are in this phase because D9 keeps them on: a child with no terminal would deny every ask after 10 minutes |
| P4 | `transport-unix` + `haest ctl list, inspect, events [--follow], send, interrupt` | from a second terminal `haest ctl events --follow` shows a live REPL turn; `send` on a watch socket is refused; a stale socket from a crashed run is replaced; the dir is 0700 and the socket 0600; agentctl's tests ported as behaviour |
| P5 | `transport-broker`: dial out, token in the first frame, backoff with full jitter, gap on overflow, stop on an auth close | against an in-test broker: cut the link mid-turn, reconnect, nothing lost while the ring holds it (B1 fixed: replay waits for the broker's ack), a gap when it does not; a wrong token stops retrying; commands obey `broker_access` |

Each phase merges to `main` on its own, gated as CONTRIBUTING says, and is useful without the
next: P1 alone gives parents a versioned, sequenced `--json`.

## 4. Not built

- Remote sub-agents (decided 2026-11-16: not needed). Where they would plug in: an `AgentDirectory`
  (`haest-tools/src/agents.rs:181-196`) over a `Backend`, so the Agent, SendMessage and
  StopAgent tools stay as they are. No phase builds it.
- The controller side: a child backend (agent-host's process host driving a child over stdio)
  and a WS backend. Both are `Backend` implementations over `haest-wire`, built only when a
  consumer needs haest to drive another agent.
- The broker server (fleet, viewers, routing). A service with its own lifecycle, not a library
  piece; protokol's and claudewerk's brokers exist. See Q9.
- A listening WebSocket server in the agent (B4); the agent dials out.
- `mcp_request/response` and `sync_check/catchup/stale`: unhandled in protokol; replay plus gap
  covers catch-up.
- A second transcript beside the history (`agent-session.ts:72-90`); `history` reads the agent's.
- agent-core's event union (budget, workflow, task events): haest's `LoopEvent` is the body.

## 5. Questions

Numbered after `durable-runs.md` (Q1-Q4) and `budgets.md` (Q5-Q6), so a code means one
question across the three plans.

- Q7: Which vocabulary is native? Recommended: haest's own envelope around `LoopEvent`, with an
  adaptor per outside protocol living with that protocol. Alternatives: agent-protocol frames
  as they are (protokol's host and controller drive haest with no adaptor, but `transcript.content`
  is untyped and approvals lose "always"), or claudewerk's messages (remote-claude's broker
  takes a Rust haest directly, but a product's protocol enters an agnostic library).
- Q8: Who is the first consumer? brain needs none of it. If it is remote-claude spawning
  `haest --stdio` behind a TS adaptor (which would retire the TS haest's loop), P3 comes right
  after P2 and P4/P5 can wait.
- Q9: The broker server: stay out of haest (recommended), or a `haest-broker` binary crate
  outside the facade?
- Q10: P2 moves the turn loop from the CLI into the library and gives haest-runtime an optional
  dependency on haest-session. Fine, given the 0.2.0 renames running in other worktrees?
- Q11: An attached control socket defaults to watch-only (D9 "brakes on"). agentctl defaults to
  command. Keep watch?
- Q12: B1-B3 are bugs in protokol. File them there?
