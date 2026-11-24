//! The forwarder: a session's stream told the client as `session/update`
//! notifications, frame by frame, in order.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use agent_client_protocol::v1::schema::{
    ConfigOptionUpdate, ErrorStopReason, IdleStateUpdate, IdleStopReason, Plan, RunningStateUpdate,
    SessionId, SessionUpdate, StateUpdate, SubagentUpdate, ToolCall as AcpCall, ToolCallStatus,
    ToolCallUpdate,
};
use haest_core::{
    AssistantPart, Effort, LoopEvent, StopReason, ToolSourceRef, task_board::TaskBoard,
};
use haest_session::{Body, Frame, Subscription};
use haest_wire::{Level, Outbound, Snapshot};
use tokio::sync::{oneshot, watch};

use crate::{
    config,
    state::Announced,
    tell::{Teller, agent_text, agent_thought},
    translate::{call_fields, new_call, plan, result_fields, tool_kind},
};

/// What one agent's stream (main's, or a subagent's) has said of the model
/// call that runs: its text and reasoning streamed, the calls it started.
/// The complete answer says only what the stream did not.
#[derive(Debug, Default)]
struct Said {
    text: bool,
    thinking: bool,
    started: HashSet<String>,
}

/// A subagent the client was told of: its child session and its parent's.
struct Child {
    session: SessionId,
    parent: SessionId,
}

/// Tells the client one session's stream.
pub(crate) struct Forwarder {
    tell: Teller,
    tools: ToolSourceRef,
    board: TaskBoard,
    cwd: PathBuf,
    announced: Announced,
    /// The model and effort the client was last told.
    shown: (String, Option<String>),
    /// The plan the client was last told.
    plan: Plan,
    /// What each agent's model call has said, by subagent id ("" for main).
    said: HashMap<String, Said>,
    /// The subagents the client was told of, by id.
    children: HashMap<String, Child>,
    /// Their child sessions, by the name a subagent's parent is given as.
    named: HashMap<String, SessionId>,
    /// The haest kind of each tool looked up so far.
    kinds: HashMap<String, String>,
}

impl Forwarder {
    /// A session's forwarder: `tools` say each call's kind, `board` is the
    /// task list, `cwd` what a relative path is against; `shown` the model
    /// and effort and `plan` the task list the client already has.
    pub(crate) fn new(
        tell: Teller,
        tools: ToolSourceRef,
        board: TaskBoard,
        cwd: PathBuf,
        announced: Announced,
        shown: (String, Option<String>),
    ) -> Self {
        let plan = plan(&board.list());
        Self {
            tell,
            tools,
            board,
            cwd,
            announced,
            shown,
            plan,
            said: HashMap::new(),
            children: HashMap::new(),
            named: HashMap::new(),
            kinds: HashMap::new(),
        }
    }

    /// Tell the client every frame of `sub` once `ready` fires (the client
    /// knows the session then), each frame's seq on `told` once it is sent.
    /// Ends with the stream, or at once when the session never opened.
    pub(crate) async fn run(
        mut self,
        mut sub: Subscription,
        ready: oneshot::Receiver<()>,
        told: watch::Sender<u64>,
    ) {
        if ready.await.is_err() {
            return;
        }
        while let Some(frame) = sub.recv().await {
            self.frame(&frame);
            let _ = told.send(frame.seq);
        }
    }

    fn frame(&mut self, frame: &Frame) {
        match frame.path.segments().nth(1) {
            None => self.main(&frame.body),
            Some(id) if self.tell.caps.subagents => self.subagent(id, &frame.body),
            // A client that takes no subagents sees each one as its tool call.
            Some(_) => {}
        }
    }

    fn main(&mut self, body: &Body) {
        match body {
            Body::Loop(ev) => {
                let session = self.tell.session.clone();
                let mut said = self.said.remove("").unwrap_or_default();
                self.event(&session, &mut said, ev, true);
                self.said.insert(String::new(), said);
            }
            Body::Wire(Outbound::Notice { level, text }) => self.tell.notice(*level, text),
            Body::Wire(Outbound::Status(now)) => self.status(now),
            // Frames were lost: each answer says all of itself again.
            Body::Wire(Outbound::Gap { .. }) => self.said.clear(),
            _ => {}
        }
    }

    /// A subagent's frame: its start and end on its parent's session, its
    /// events on its own child session.
    fn subagent(&mut self, id: &str, body: &Body) {
        match body {
            Body::Wire(Outbound::SubagentStarted {
                name,
                parent,
                description,
            }) => {
                let parent = self.named.get(parent).cloned();
                let parent = parent.unwrap_or_else(|| self.tell.session.clone());
                let session = SessionId::from(format!("{}:{id}", self.tell.session.0));
                let started = SubagentUpdate::new(session.clone())
                    .title(name.clone())
                    .description(description.clone())
                    .state(StateUpdate::Running(RunningStateUpdate::new()));
                self.tell
                    .send(&parent, SessionUpdate::SubagentUpdate(started));
                self.named.insert(name.clone(), session.clone());
                self.children
                    .insert(id.to_string(), Child { session, parent });
            }
            Body::Wire(Outbound::SubagentStopped { ok }) => {
                let Some(child) = self.children.get(id) else {
                    return;
                };
                let reason = if *ok {
                    IdleStopReason::EndTurn
                } else {
                    IdleStopReason::Error(ErrorStopReason::new())
                };
                let idle = IdleStateUpdate::new().stop_reason(reason);
                let stopped =
                    SubagentUpdate::new(child.session.clone()).state(StateUpdate::Idle(idle));
                self.tell
                    .send(&child.parent, SessionUpdate::SubagentUpdate(stopped));
            }
            Body::Loop(ev) => {
                let Some(child) = self.children.get(id) else {
                    return;
                };
                let session = child.session.clone();
                let mut said = self.said.remove(id).unwrap_or_default();
                self.event(&session, &mut said, ev, false);
                self.said.insert(id.to_string(), said);
            }
            _ => {}
        }
    }

    /// A status frame: the client is told a model or effort it has not seen.
    fn status(&mut self, now: &Snapshot) {
        let shown = (now.model_spec.clone(), now.effort.clone());
        if shown == self.shown {
            return;
        }
        let effort = now.effort.as_deref().and_then(|e| e.parse::<Effort>().ok());
        let options = config::options(&now.model_spec, effort);
        self.shown = shown;
        let update = SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(options));
        self.tell.send(&self.tell.session, update);
    }

    /// The task list, when it is not what the client was last told.
    fn task_list(&mut self) {
        let now = plan(&self.board.list());
        if now != self.plan {
            self.plan = now.clone();
            self.tell.send(&self.tell.session, SessionUpdate::Plan(now));
        }
    }

    /// The haest kind of the tool `name`; empty for a tool not found.
    fn kind(&mut self, name: &str) -> String {
        if let Some(kind) = self.kinds.get(name) {
            return kind.clone();
        }
        let tools = self.tools.callable();
        let kind = tools
            .get(name)
            .map(|t| t.kind().to_string())
            .unwrap_or_default();
        self.kinds.insert(name.to_string(), kind.clone());
        kind
    }

    /// One loop event of an agent whose updates go to `session`; `main`
    /// for the conversation's own (the task list and a turn's end are its).
    fn event(&mut self, session: &SessionId, said: &mut Said, ev: &LoopEvent, main: bool) {
        match ev {
            LoopEvent::ModelStart { .. } => *said = Said::default(),
            LoopEvent::TextDelta { text } => {
                said.text = true;
                self.tell.send(session, agent_text(text.clone()));
            }
            LoopEvent::ThinkingDelta { text } => {
                said.thinking = true;
                self.tell.send(session, agent_thought(text.clone()));
            }
            LoopEvent::ToolCallStart { id, name } => {
                let kind = tool_kind(&self.kind(name));
                let call = AcpCall::new(id.clone(), name.clone())
                    .kind(kind)
                    .status(ToolCallStatus::Pending);
                said.started.insert(id.clone());
                self.announced.insert(id);
                self.tell.send(session, SessionUpdate::ToolCall(call));
            }
            LoopEvent::Assistant { message } => {
                if !said.thinking {
                    let thought: String = message
                        .content
                        .iter()
                        .filter_map(|p| match p {
                            AssistantPart::Thinking { thinking, .. } => Some(thinking.as_str()),
                            _ => None,
                        })
                        .collect();
                    if !thought.is_empty() {
                        self.tell.send(session, agent_thought(thought));
                    }
                }
                let text = message.text();
                if !said.text && !text.is_empty() {
                    self.tell.send(session, agent_text(text));
                }
                for call in message.tool_calls() {
                    let kind = self.kind(&call.name);
                    let running = ToolCallStatus::InProgress;
                    let update = if said.started.contains(&call.id) {
                        let fields = call_fields(call, &kind, &self.cwd).status(running);
                        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(call.id.clone(), fields))
                    } else {
                        self.announced.insert(&call.id);
                        SessionUpdate::ToolCall(new_call(call, &kind, &self.cwd, running))
                    };
                    self.tell.send(session, update);
                }
            }
            LoopEvent::ToolResult {
                call,
                message,
                output,
                ..
            } => {
                let kind = self.kind(&call.name);
                if !self.announced.contains(&call.id) {
                    self.announced.insert(&call.id);
                    let entry = new_call(call, &kind, &self.cwd, ToolCallStatus::InProgress);
                    self.tell.send(session, SessionUpdate::ToolCall(entry));
                }
                let fields = result_fields(
                    &kind,
                    message.is_error,
                    &message.content,
                    output.result.as_ref(),
                    output.title.as_deref(),
                );
                let update = ToolCallUpdate::new(call.id.clone(), fields);
                self.tell
                    .send(session, SessionUpdate::ToolCallUpdate(update));
                if main && kind == "todo.write" {
                    self.task_list();
                }
            }
            LoopEvent::Guard { text } if main => self.tell.notice(Level::Warning, text),
            // A cap or the budget stopped the turn: the human hears why. A
            // turn that failed says so in the prompt's answer instead.
            LoopEvent::TurnEnd {
                stop_reason,
                error: Some(error),
                ..
            } if main && !matches!(stop_reason, StopReason::Error | StopReason::Aborted) => {
                self.tell.notice(Level::Warning, error);
            }
            // The user's own message (the client shows what it sent), a
            // subagent's guard and turn end, an event newer than this crate.
            _ => {}
        }
    }
}
