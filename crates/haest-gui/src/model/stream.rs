//! One agent's conversation as the stream view shows it: a list of items,
//! built from the frames as they arrive.
//!
//! A model call's text streams in as deltas, but the ring drops those once
//! the call's `assistant` frame is in (a late peer gets the `assistant`
//! alone), so the `assistant` frame is the truth: it replaces whatever the
//! deltas built for that call.

use haest::core::{
    LoopEvent,
    messages::{AssistantMessage, AssistantPart, Message, StopReason, ToolCall, Usage},
};
use serde_json::{Map, Value};

/// How much a notice matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    /// Worth a muted line.
    Info,
    /// Something did not go as asked.
    Warning,
    /// Something failed.
    Error,
}

/// A tool call's state.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    /// It runs (or the model is still writing its input).
    Running,
    /// It answered; `ms` is None when the answer came from history.
    Done {
        /// How long it ran.
        ms: Option<u64>,
        /// It failed.
        error: bool,
    },
}

/// One tool call, as a card.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCard {
    /// The call's id, as the model and haest's frames name it.
    pub call_id: String,
    /// The tool's name.
    pub name: String,
    /// Its input, once the model finished writing it.
    pub input: Option<Map<String, Value>>,
    /// One line that says what the call does.
    pub preview: String,
    /// Where it is.
    pub status: ToolStatus,
    /// What the model reads back.
    pub result: Option<String>,
    /// The tool's own title for the finished call, when it has one.
    pub title: Option<String>,
    /// Which batch of the turn it ran in, and its place in that batch.
    pub batch: usize,
    /// How many calls that batch has.
    pub batch_size: usize,
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    /// Why it stopped, haest's word for it (`end_turn`, `aborted`).
    pub stop_reason: String,
    /// How long it took.
    pub duration_ms: u64,
    /// Its tokens and cost, summed over its model calls.
    pub usage: Usage,
    /// What failed, when it ended on an error.
    pub error: Option<String>,
}

/// A subagent started or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPhase {
    /// It runs.
    Started,
    /// It finished its work.
    Finished,
    /// It stopped before finishing.
    StoppedEarly,
}

/// What an item is.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    /// The input that started or steered a turn.
    User {
        /// What was sent.
        text: String,
    },
    /// The model's text.
    Answer {
        /// The text so far, markdown.
        text: String,
        /// Still streaming.
        live: bool,
    },
    /// The model's reasoning.
    Thinking {
        /// The text so far.
        text: String,
        /// Still streaming.
        live: bool,
    },
    /// A tool call.
    Tool(ToolCard),
    /// haest's own line, or one the window adds.
    Notice {
        /// How much it matters.
        level: NoticeLevel,
        /// The line.
        text: String,
    },
    /// The guard's nudge to the model.
    Guard {
        /// The nudge.
        text: String,
    },
    /// A subagent started or stopped.
    Agent {
        /// Its id.
        id: String,
        /// Its address.
        name: String,
        /// What it was asked to do.
        description: String,
        /// Where it is.
        phase: AgentPhase,
    },
    /// The turn ended.
    TurnEnd(TurnSummary),
    /// Frames were lost; what is above came from `history`.
    Lost {
        /// How many, when the haest said.
        missed: Option<u64>,
        /// `history` has answered.
        caught_up: bool,
    },
}

/// One row of the stream. `uid` names it for as long as it lives (element
/// ids, a reader's open/closed choice); `rev` changes whenever it does.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Its name for as long as it lives.
    pub uid: u64,
    /// Changes whenever it does.
    pub rev: u64,
    /// What it is.
    pub kind: ItemKind,
}

/// One agent's stream.
#[derive(Debug, Default, Clone)]
pub struct Stream {
    /// Its rows, oldest first.
    pub items: Vec<Item>,
    /// Where the running model call's items begin.
    call_start: Option<usize>,
    /// Batches so far in this stream (a batch = the tool calls of one answer).
    batches: usize,
    next_uid: u64,
    next_rev: u64,
    /// A model call is streaming now.
    pub streaming: bool,
}

impl Stream {
    /// An empty stream.
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, kind: ItemKind) {
        self.next_uid += 1;
        self.next_rev += 1;
        self.items.push(Item {
            uid: self.next_uid,
            rev: self.next_rev,
            kind,
        });
    }

    fn touch(&mut self, ix: usize) {
        self.next_rev += 1;
        self.items[ix].rev = self.next_rev;
    }

    /// Every live text block is done.
    fn close_live(&mut self) {
        for ix in 0..self.items.len() {
            let changed = match &mut self.items[ix].kind {
                ItemKind::Answer { live, .. } | ItemKind::Thinking { live, .. } if *live => {
                    *live = false;
                    true
                }
                _ => false,
            };
            if changed {
                self.touch(ix);
            }
        }
    }

    /// A line for the human.
    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.push(ItemKind::Notice {
            level,
            text: text.into(),
        });
    }

    /// A subagent of this agent started or stopped: one row per subagent,
    /// its phase moved on in place.
    pub fn agent(&mut self, id: &str, name: &str, description: &str, phase: AgentPhase) {
        let row = self
            .items
            .iter()
            .rposition(|i| matches!(&i.kind, ItemKind::Agent { id: x, .. } if x == id));
        if let Some(ix) = row {
            if let ItemKind::Agent { phase: p, .. } = &mut self.items[ix].kind {
                *p = phase;
            }
            self.touch(ix);
            return;
        }
        self.push(ItemKind::Agent {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            phase,
        });
    }

    /// Frames were lost here.
    pub fn lost(&mut self, missed: Option<u64>) {
        self.close_live();
        self.call_start = None;
        self.streaming = false;
        self.push(ItemKind::Lost {
            missed,
            caught_up: false,
        });
    }

    /// One of the conversation's events.
    pub fn event(&mut self, ev: &LoopEvent) {
        match ev {
            LoopEvent::User { message } => {
                self.close_live();
                self.push(ItemKind::User {
                    text: message.content.clone(),
                });
            }
            LoopEvent::ModelStart { .. } => {
                self.close_live();
                self.call_start = Some(self.items.len());
                self.streaming = true;
            }
            LoopEvent::TextDelta { text } => self.delta(text, false),
            LoopEvent::ThinkingDelta { text } => self.delta(text, true),
            LoopEvent::ToolCallStart { id, name } => {
                self.close_live();
                self.call_start.get_or_insert(self.items.len());
                self.push(ItemKind::Tool(ToolCard {
                    call_id: id.clone(),
                    name: name.clone(),
                    input: None,
                    preview: String::new(),
                    status: ToolStatus::Running,
                    result: None,
                    title: None,
                    batch: self.batches,
                    batch_size: 0,
                }));
            }
            LoopEvent::Assistant { message } => self.answer(message),
            LoopEvent::ToolResult {
                call,
                message,
                output,
                duration_ms,
            } => {
                let error = output.is_error || message.is_error;
                let status = ToolStatus::Done {
                    ms: Some(*duration_ms),
                    error,
                };
                self.result(call, &message.content, output.title.clone(), status);
            }
            LoopEvent::Guard { text } => self.push(ItemKind::Guard { text: text.clone() }),
            LoopEvent::TurnEnd {
                stop_reason,
                usage,
                duration_ms,
                error,
            } => {
                self.close_live();
                self.call_start = None;
                self.streaming = false;
                self.push(ItemKind::TurnEnd(TurnSummary {
                    stop_reason: stop_reason.as_str().to_string(),
                    duration_ms: *duration_ms,
                    usage: usage.clone(),
                    error: error.clone(),
                }));
            }
            // A newer event this build does not know: nothing to show.
            _ => {}
        }
    }

    fn delta(&mut self, text: &str, thinking: bool) {
        self.call_start.get_or_insert(self.items.len());
        let last = self.items.len().checked_sub(1);
        if let Some(ix) = last {
            let grew = match &mut self.items[ix].kind {
                ItemKind::Answer {
                    text: t,
                    live: true,
                } if !thinking => {
                    t.push_str(text);
                    true
                }
                ItemKind::Thinking {
                    text: t,
                    live: true,
                } if thinking => {
                    t.push_str(text);
                    true
                }
                _ => false,
            };
            if grew {
                self.touch(ix);
                return;
            }
        }
        self.close_live();
        let text = text.to_string();
        self.push(if thinking {
            ItemKind::Thinking { text, live: true }
        } else {
            ItemKind::Answer { text, live: true }
        });
    }

    /// The model call's whole answer: it replaces what its deltas built.
    fn answer(&mut self, message: &AssistantMessage) {
        if let Some(start) = self.call_start.take() {
            self.items.truncate(start);
        }
        self.streaming = false;
        self.parts(message);
        if message.stop_reason == StopReason::Error {
            let why = message
                .error_message
                .clone()
                .unwrap_or_else(|| "the model call failed".into());
            self.notice(NoticeLevel::Error, why);
        }
    }

    /// An answer's parts as items; its tool calls are one batch.
    fn parts(&mut self, message: &AssistantMessage) {
        let calls = message.tool_calls().count();
        let batch = self.batches;
        if calls > 0 {
            self.batches += 1;
        }
        for part in &message.content {
            match part {
                AssistantPart::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                    self.push(ItemKind::Thinking {
                        text: thinking.clone(),
                        live: false,
                    });
                }
                AssistantPart::Text { text } if !text.trim().is_empty() => {
                    self.push(ItemKind::Answer {
                        text: text.clone(),
                        live: false,
                    });
                }
                AssistantPart::ToolCall(call) => {
                    self.push(ItemKind::Tool(ToolCard {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        preview: input_preview(&call.name, &call.input),
                        input: Some(call.input.clone()),
                        status: ToolStatus::Running,
                        result: None,
                        title: None,
                        batch,
                        batch_size: calls,
                    }));
                }
                _ => {}
            }
        }
    }

    /// A tool call's answer lands on its card (a card is made when the call
    /// was never seen: it came before this peer joined).
    fn result(
        &mut self,
        call: &ToolCall,
        content: &str,
        title: Option<String>,
        status: ToolStatus,
    ) {
        let found = self
            .items
            .iter()
            .rposition(|it| matches!(&it.kind, ItemKind::Tool(c) if c.call_id == call.id));
        let ix = match found {
            Some(ix) => ix,
            None => {
                self.push(ItemKind::Tool(ToolCard {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    preview: input_preview(&call.name, &call.input),
                    input: Some(call.input.clone()),
                    status: ToolStatus::Running,
                    result: None,
                    title: None,
                    batch: self.batches,
                    batch_size: 1,
                }));
                self.batches += 1;
                self.items.len() - 1
            }
        };
        if let ItemKind::Tool(card) = &mut self.items[ix].kind {
            card.status = status;
            card.result = Some(content.to_string());
            card.title = title;
            if card.input.is_none() {
                card.preview = input_preview(&call.name, &call.input);
                card.input = Some(call.input.clone());
            }
        }
        self.touch(ix);
    }

    /// The conversation as `history` has it: the stream starts over from
    /// its messages, and says that frames were lost.
    pub fn from_history(messages: &[Message], missed: Option<u64>) -> Self {
        let mut s = Self::new();
        for message in messages {
            match message {
                Message::User(u) => s.push(ItemKind::User {
                    text: u.content.clone(),
                }),
                Message::Assistant(a) => s.parts(a),
                Message::Tool(t) => {
                    let call = ToolCall {
                        id: t.tool_call_id.clone(),
                        name: t.tool_name.clone(),
                        input: Map::new(),
                    };
                    let status = ToolStatus::Done {
                        ms: None,
                        error: t.is_error,
                    };
                    s.result(&call, &t.content, None, status);
                }
                _ => {}
            }
        }
        s.push(ItemKind::Lost {
            missed,
            caught_up: true,
        });
        s
    }
}

/// One line that says what a call does: a shell command whole, a path, or
/// the input as compact JSON.
pub fn input_preview(tool: &str, input: &Map<String, Value>) -> String {
    const KEYS: [&str; 8] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ];
    let lower = tool.to_ascii_lowercase();
    let first = if lower == "bash" || lower == "shell" {
        input.get("command").and_then(Value::as_str)
    } else {
        KEYS.iter()
            .find_map(|k| input.get(*k).and_then(Value::as_str))
    };
    let line = match first {
        Some(s) => s.to_string(),
        None if input.is_empty() => String::new(),
        None => Value::Object(input.clone()).to_string(),
    };
    one_line(&line, 240)
}

/// `text` on one line, at most `max` chars, an ellipsis when cut.
pub fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() <= max {
        return flat.to_string();
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}
