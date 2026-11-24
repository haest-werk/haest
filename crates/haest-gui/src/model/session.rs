//! One attached haest: its frames reduced to what the views show.

use std::collections::BTreeMap;

use haest::core::{LoopEvent, messages::Message};
use haest::session::{Body, Frame};
use haest::wire::{Access, Answer, Answerer, Level, Outbound, State};

use super::{
    agents::{AgentTree, MAIN},
    stream::{AgentPhase, NoticeLevel, Stream},
    timeline::Timeline,
};

/// The connection to it.
#[derive(Debug, Clone, PartialEq)]
pub enum Conn {
    /// Asked to follow it; its `hello` has not arrived.
    Connecting,
    /// Following its stream.
    Live,
    /// The socket went away (or never answered), and why.
    Lost(String),
}

/// Which ask: a peer answers an approval ask; a terminal ask is answered on
/// that haest's own terminal (haest-gui only shows it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AskId {
    /// An approval ask, by the number haest gave it.
    Approval(u64),
    /// A terminal's question, by the seq of the frame that asked it.
    Terminal(u64),
}

/// One ask waiting for a human.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    /// Which ask.
    pub id: AskId,
    /// The tool that asks (empty for a terminal's own question).
    pub tool_name: String,
    /// The tool call it holds up, when the haest names it.
    pub tool_call_id: Option<String>,
    /// Why it asks, in haest's words.
    pub description: String,
    /// The call's input, as the human should see it (a command whole).
    pub preview: String,
    /// What "always" would allow from now on, when it can.
    pub key: Option<String>,
    /// When it arrived, ms since the epoch.
    pub arrived_ts: u64,
    /// The haest asks the same at its own terminal: the first answer counts.
    pub at_terminal: bool,
    /// The answer haest-gui sent, while the haest has not confirmed it.
    pub answering: Option<Answer>,
}

/// One attached haest.
#[derive(Debug, Clone)]
pub struct SessionModel {
    /// The stream id: the socket's name, fixed for the haest's life.
    pub sid: String,
    /// The session file's id now (a cleared conversation has a new one).
    pub session: String,
    /// The model it runs, as `provider:model`.
    pub model: String,
    /// Its process id.
    pub pid: u32,
    /// What its socket lets a peer do.
    pub access: Access,
    /// When it started, ms since the epoch.
    pub started_ms: u64,
    /// Running a turn, or idle.
    pub state: State,
    /// The effort it runs at; `None` is the model's own default.
    pub effort: Option<String>,
    /// What the conversation has cost so far, when the provider says.
    pub usd: Option<f64>,
    /// Inputs waiting for the next turn.
    pub queued: usize,
    /// The conversation's message count.
    pub messages: usize,
    /// The connection to it.
    pub conn: Conn,
    /// Main and its subagents.
    pub tree: AgentTree,
    /// Each agent's stream, by agent id.
    pub streams: BTreeMap<String, Stream>,
    /// Each agent's turns, by agent id.
    pub timelines: BTreeMap<String, Timeline>,
    /// The asks waiting for a human, oldest first.
    pub asks: Vec<Ask>,
    /// The last stream frame applied.
    pub last_seq: u64,
    /// Frames up to this seq are already in the history the stream was
    /// rebuilt from.
    ignore_through: u64,
    /// A gap arrived and `history` has not yet answered: the frames lost.
    pub gap: Option<Option<u64>>,
    /// The newest frame's time: "now" for a stream that is replayed.
    pub last_ts: u64,
}

impl SessionModel {
    /// A haest just attached to: nothing seen yet.
    pub fn new(sid: impl Into<String>, access: Access) -> Self {
        let mut streams = BTreeMap::new();
        streams.insert(MAIN.to_string(), Stream::new());
        let mut timelines = BTreeMap::new();
        timelines.insert(MAIN.to_string(), Timeline::new());
        Self {
            sid: sid.into(),
            session: String::new(),
            model: String::new(),
            pid: 0,
            access,
            started_ms: 0,
            state: State::Idle,
            effort: None,
            usd: None,
            queued: 0,
            messages: 0,
            conn: Conn::Connecting,
            tree: AgentTree::new(),
            streams,
            timelines,
            asks: Vec::new(),
            last_seq: 0,
            ignore_through: 0,
            gap: None,
            last_ts: 0,
        }
    }

    /// The short id people read: the first 8 chars of the stream id.
    pub fn short_id(&self) -> &str {
        short(&self.sid)
    }

    /// Whether a request can go to it now: a command socket, connected.
    pub fn can_command(&self) -> bool {
        self.access == Access::Command && self.conn == Conn::Live
    }

    /// `agent`'s stream, once it has one.
    pub fn stream(&self, agent: &str) -> Option<&Stream> {
        self.streams.get(agent)
    }

    fn stream_mut(&mut self, agent: &str) -> &mut Stream {
        self.streams.entry(agent.to_string()).or_default()
    }

    fn timeline_mut(&mut self, agent: &str) -> &mut Timeline {
        self.timelines.entry(agent.to_string()).or_default()
    }

    /// One frame from the stream. Connection frames other than `gap` are
    /// the link's business and change nothing here; a frame already folded
    /// into history, or already applied, is skipped.
    pub fn apply(&mut self, frame: &Frame) -> bool {
        if let Body::Wire(Outbound::Gap { missed, .. }) = &frame.body {
            self.gap = Some(*missed);
            self.stream_mut(MAIN).lost(*missed);
            for timeline in self.timelines.values_mut() {
                timeline.lost();
            }
            return true;
        }
        if matches!(
            frame.body,
            Body::Wire(Outbound::Hello { .. } | Outbound::Reply { .. })
        ) {
            return false;
        }
        if frame.seq <= self.ignore_through || frame.seq <= self.last_seq {
            return false;
        }
        self.last_seq = frame.seq;
        self.last_ts = self.last_ts.max(frame.ts);
        let agent = agent_of(frame);
        match &frame.body {
            Body::Loop(ev) => self.event(&agent, ev, frame.ts),
            Body::Wire(w) => self.wire(&agent, w, frame),
        }
        true
    }

    fn event(&mut self, agent: &str, ev: &LoopEvent, ts: u64) {
        match ev {
            LoopEvent::ModelStart { .. } => self.tree.streaming(agent, true),
            LoopEvent::Assistant { .. } | LoopEvent::TurnEnd { .. } => {
                self.tree.streaming(agent, false)
            }
            LoopEvent::ToolResult { .. } if agent == MAIN => {
                // A question only the terminal asks (a haest whose socket
                // watches) is answered once a tool runs on: the wire says
                // nothing else of it.
                if let Some(ix) = self
                    .asks
                    .iter()
                    .position(|a| matches!(a.id, AskId::Terminal(_)))
                {
                    self.asks.remove(ix);
                }
            }
            _ => {}
        }
        if agent == MAIN && matches!(ev, LoopEvent::TurnEnd { .. }) {
            // Nor does it outlive its turn. An approval ask stays: the wire
            // ends each one (a subagent's may outlive main's turn).
            self.asks.retain(|a| matches!(a.id, AskId::Approval(_)));
        }
        self.stream_mut(agent).event(ev);
        self.timeline_mut(agent).event(ev, ts);
    }

    fn wire(&mut self, agent: &str, w: &Outbound, frame: &Frame) {
        match w {
            Outbound::Notice { level, text } => {
                let level = match level {
                    Level::Warning => NoticeLevel::Warning,
                    Level::Error => NoticeLevel::Error,
                    _ => NoticeLevel::Info,
                };
                // The subagent's own row and the Agents tab say this already.
                if !is_subagent_count(text) {
                    self.stream_mut(agent).notice(level, text.clone());
                }
            }
            // The terminal's side of an approval ask: the same ask, which
            // either side may answer.
            Outbound::Ask {
                approval: Some(n), ..
            } => {
                if let Some(a) = self.asks.iter_mut().find(|a| a.id == AskId::Approval(*n)) {
                    a.at_terminal = true;
                }
            }
            Outbound::Ask { text, .. } => {
                self.asks.push(Ask {
                    id: AskId::Terminal(frame.seq),
                    tool_name: String::new(),
                    tool_call_id: None,
                    description: text.clone(),
                    preview: String::new(),
                    key: None,
                    arrived_ts: frame.ts,
                    at_terminal: true,
                    answering: None,
                });
                let line = format!("Asked on its own terminal: {}", text.trim());
                self.stream_mut(agent).notice(NoticeLevel::Warning, line);
            }
            Outbound::Status(now) => {
                self.state = now.state;
                self.session = now.session.clone();
                self.model = now.model_spec.clone();
                self.effort = now.effort.clone();
                self.usd = now.usd;
                self.queued = now.queued;
                self.messages = now.messages;
            }
            Outbound::ApprovalAsk {
                ask,
                tool_name,
                tool_call_id,
                description,
                preview,
                key,
            } => {
                self.asks.push(Ask {
                    id: AskId::Approval(*ask),
                    tool_name: tool_name.clone(),
                    tool_call_id: tool_call_id.clone(),
                    description: description.clone(),
                    preview: preview.clone(),
                    key: key.clone(),
                    arrived_ts: frame.ts,
                    at_terminal: false,
                    answering: None,
                });
                let line = format!("{tool_name} asks for approval: {description}");
                self.stream_mut(agent).notice(NoticeLevel::Warning, line);
            }
            Outbound::ApprovalWithdrawn { ask } => {
                self.asks.retain(|a| a.id != AskId::Approval(*ask));
                let line = format!("Approval ask {ask} withdrawn");
                self.stream_mut(agent).notice(NoticeLevel::Info, line);
            }
            Outbound::ApprovalAnswered { ask, answer, by } => {
                let id = AskId::Approval(*ask);
                let mine = self.asks.iter().find(|a| a.id == id);
                let sent = mine.is_some_and(|a| a.answering.is_some());
                let tool =
                    mine.map_or_else(|| format!("Approval ask {ask}"), |a| a.tool_name.clone());
                self.asks.retain(|a| a.id != id);
                let who = match by {
                    Answerer::Terminal => "at its terminal",
                    _ if sent => "here",
                    _ => "by another program",
                };
                let line = format!("{tool}: {} {who}", answered_word(*answer));
                self.stream_mut(agent).notice(NoticeLevel::Info, line);
            }
            Outbound::SubagentStarted {
                name,
                parent,
                description,
            } => {
                self.tree
                    .started(agent, name, parent, description, frame.ts);
                let parent_id = self
                    .tree
                    .get(agent)
                    .and_then(|n| n.parent.clone())
                    .unwrap_or_else(|| MAIN.into());
                self.stream_mut(agent);
                self.timeline_mut(agent);
                self.stream_mut(&parent_id)
                    .agent(agent, name, description, AgentPhase::Started);
            }
            Outbound::SubagentStopped { ok } => {
                self.tree.stopped(agent, *ok, frame.ts);
                let (name, description, parent_id) = match self.tree.get(agent) {
                    Some(n) => (
                        n.name.clone(),
                        n.description.clone(),
                        n.parent.clone().unwrap_or_else(|| MAIN.into()),
                    ),
                    None => (agent.to_string(), String::new(), MAIN.into()),
                };
                let phase = if *ok {
                    AgentPhase::Finished
                } else {
                    AgentPhase::StoppedEarly
                };
                self.stream_mut(&parent_id)
                    .agent(agent, &name, &description, phase);
            }
            _ => {}
        }
    }

    /// `history`'s answer to a gap: main's stream starts over from its
    /// messages, and frames up to `seq` are skipped from now on.
    pub fn apply_history(&mut self, seq: u64, messages: &[Message]) {
        let missed = self.gap.take().flatten();
        self.streams
            .insert(MAIN.to_string(), Stream::from_history(messages, missed));
        self.ignore_through = self.ignore_through.max(seq);
        self.last_seq = self.last_seq.max(seq);
    }

    /// haest-gui answered an ask; it stays, marked, until the haest confirms.
    pub fn answering(&mut self, ask: AskId, answer: Answer) {
        if let Some(a) = self.asks.iter_mut().find(|a| a.id == ask) {
            a.answering = Some(answer);
        }
    }

    /// The haest took the answer, or refused it (answered elsewhere,
    /// withdrawn): either way the ask is gone.
    pub fn answered(&mut self, ask: AskId) {
        self.asks.retain(|a| a.id != ask);
    }

    /// The approval asks haest-gui may answer here (command access, live).
    pub fn answerable(&self, ask: &Ask) -> bool {
        matches!(ask.id, AskId::Approval(_)) && self.can_command()
    }

    /// An approval ask waits on the tool call `call_id` (and haest-gui has not
    /// answered it yet): its card says so.
    pub fn holds(&self, call_id: &str) -> bool {
        self.asks
            .iter()
            .any(|a| a.tool_call_id.as_deref() == Some(call_id) && a.answering.is_none())
    }
}

/// `answer` as a past tense: "allowed", "allowed always", "denied".
fn answered_word(answer: Answer) -> &'static str {
    match answer {
        Answer::Allow => "allowed",
        Answer::Always => "allowed always",
        _ => "denied",
    }
}

/// Who a frame is about: `main`, or the subagent's id (its path's last segment).
pub fn agent_of(frame: &Frame) -> String {
    frame.path.segments().last().unwrap_or(MAIN).to_string()
}

/// The first 8 chars of an id.
pub fn short(id: &str) -> &str {
    let end = id.char_indices().nth(8).map_or(id.len(), |(i, _)| i);
    &id[..end]
}

/// Every ask waiting across `sessions`, oldest first, with its session.
pub fn approval_queue<'a>(
    sessions: impl IntoIterator<Item = &'a SessionModel>,
) -> Vec<(&'a SessionModel, &'a Ask)> {
    let mut queue: Vec<_> = sessions
        .into_iter()
        .flat_map(|s| s.asks.iter().map(move |a| (s, a)))
        .collect();
    queue.sort_by_key(|(s, a)| (a.arrived_ts, s.sid.clone()));
    queue
}

/// haest's `Subagents running: N` notice.
fn is_subagent_count(text: &str) -> bool {
    text.strip_prefix("Subagents running: ")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}
