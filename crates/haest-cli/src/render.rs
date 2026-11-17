//! Turning the session's stream into terminal output.
//!
//! stdout carries the ANSWER only (the model's text), so `haest -p ... > out.md`
//! captures exactly that. Everything about the work -- tool calls, thinking,
//! guard notices, errors, the cost line, subagents -- goes to stderr. `--json`
//! replaces all of it with one JSON line per frame on stdout, each in
//! haest-wire's envelope (`v`, `sid`, `seq`, `ts`, `path`, then the frame's
//! own fields).

use std::{
    io::{IsTerminal, Write},
    sync::mpsc::Sender,
};

use haest::{
    LoopEvent, StopReason, ToolCall, Usage,
    session::{Body, Frame},
    wire::{Framer, Outbound, Path, State, encode_line},
};
use serde::Serialize;
use serde_json::Value;

pub use haest::wire::Level;

use crate::agents::AgentLog;

/// How events are written.
pub struct Renderer<O: Write, E: Write> {
    out: O,
    err: E,
    json: bool,
    color: bool,
    /// The last thing written to stdout did not end a line.
    open_line: bool,
    /// The failed model call this turn already showed: its turn end does not
    /// say it again.
    shown_error: Option<String>,
    /// While the REPL prompt is open: where a line goes so it lands ABOVE the
    /// prompt, never inside what the human is typing.
    above: Option<Sender<String>>,
    /// Numbers the json lines written here directly (not from a stream).
    framer: Framer,
    /// Subagents' names, for their lines.
    agents: AgentLog,
    /// The last turn's usage, for the cost line once the turn is over.
    usage: Option<Usage>,
    /// Inputs waiting for a turn, as the last status said.
    queued: usize,
    /// The answer's text was written as it streamed (its pieces seen).
    streamed: bool,
    /// Write each input that enters the conversation (a watcher's view; at
    /// the terminal the human typed it).
    inputs: bool,
}

const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

impl Renderer<std::io::Stdout, std::io::Stderr> {
    pub fn terminal(json: bool) -> Self {
        let color = std::io::stderr().is_terminal();
        Renderer::new(std::io::stdout(), std::io::stderr(), json, color)
    }
}

impl<O: Write, E: Write> Renderer<O, E> {
    pub fn new(out: O, err: E, json: bool, color: bool) -> Self {
        Self {
            out,
            err,
            json,
            color,
            open_line: false,
            shown_error: None,
            above: None,
            framer: Framer::new(uuid::Uuid::new_v4().to_string()),
            agents: AgentLog::default(),
            usage: None,
            queued: 0,
            streamed: false,
            inputs: false,
        }
    }

    /// Also write each input that enters the conversation: `haest ctl
    /// events` watches a conversation someone else drives.
    pub fn showing_input(mut self) -> Self {
        self.inputs = true;
        self
    }

    /// One frame of the session's stream: in json mode its line as it is;
    /// otherwise what the human reads of it. The cost line comes after a
    /// turn of the conversation's own, with the session's total from the
    /// status that follows it.
    pub fn frame(&mut self, frame: &Frame) {
        if self.json {
            let _ = self.out.write_all(encode_line(frame).as_bytes());
            let _ = self.out.flush();
            return;
        }
        let subagent = frame.path.0.get(1).map(String::as_str);
        match (&frame.body, subagent) {
            (Body::Loop(ev), None) => {
                if let LoopEvent::TurnEnd { usage, .. } = ev {
                    self.usage = Some(usage.clone());
                }
                self.event(ev);
            }
            (Body::Loop(ev), Some(id)) => {
                if let Some((level, text)) = self.agents.loop_event(id, ev) {
                    self.notice(level, &text);
                }
            }
            (Body::Wire(wire), subagent) => self.wire(wire, subagent),
        }
    }

    /// One of the wire's own frames, in text mode.
    fn wire(&mut self, frame: &Outbound, subagent: Option<&str>) {
        match (frame, subagent) {
            (Outbound::Notice { level, text }, _) => self.notice(*level, text),
            (Outbound::Ask { text }, _) => self.ask(text),
            (Outbound::Status(now), None) => {
                // Only another program queues input while the prompt is
                // open (the human's own line waits for it to close).
                if now.queued > self.queued && self.prompting() {
                    self.notice(
                        Level::Info,
                        "Input from another program waits: haest runs it when you press Enter.",
                    );
                }
                self.queued = now.queued;
                if now.state == State::Idle
                    && let Some(usage) = self.usage.take()
                {
                    self.usage(&usage, now.usd);
                }
            }
            // A watcher's view of an ask another peer answers.
            (
                Outbound::ApprovalAsk {
                    ask,
                    tool_name,
                    description,
                    ..
                },
                _,
            ) => self.notice(
                Level::Warning,
                &format!("{tool_name} asks for approval: {description} (ask {ask})"),
            ),
            (Outbound::ApprovalWithdrawn { ask }, _) => {
                self.note(DIM, &format!("(approval ask {ask} withdrawn)"));
            }
            (
                Outbound::SubagentStarted {
                    name,
                    parent,
                    description,
                },
                Some(id),
            ) => {
                let (level, text) = self.agents.started(id, name, parent, description);
                self.notice(level, &text);
            }
            (Outbound::SubagentStopped { ok }, Some(id)) => {
                let (level, text) = self.agents.stopped(id, *ok);
                self.notice(level, &text);
            }
            (Outbound::Gap { missed, .. }, _) => {
                let n = missed.map_or("Some".to_string(), |n| n.to_string());
                self.note(
                    DIM,
                    &format!("({n} events not shown: the terminal fell behind)"),
                );
            }
            _ => {}
        }
    }

    /// The prompt opened (Some: its printer) or closed (None).
    pub fn set_above(&mut self, above: Option<Sender<String>>) {
        self.above = above;
    }

    /// The REPL prompt is open: the human is (or may be) typing.
    pub fn prompting(&self) -> bool {
        self.above.is_some()
    }

    fn paint(&self, color: &str, text: &str) -> String {
        if self.color {
            format!("{color}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    fn end_line(&mut self) {
        if self.open_line {
            let _ = writeln!(self.out);
            let _ = self.out.flush();
            self.open_line = false;
        }
    }

    fn note(&mut self, color: &str, text: &str) {
        self.end_line();
        let line = self.paint(color, text);
        match &self.above {
            Some(above) if above.send(line.clone()).is_ok() => {}
            _ => {
                let _ = writeln!(self.err, "{line}");
            }
        }
    }

    /// Answer text, to stdout as it comes.
    fn answer(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let _ = write!(self.out, "{text}");
        let _ = self.out.flush();
        self.open_line = !text.ends_with('\n');
    }

    pub fn event(&mut self, ev: &LoopEvent) {
        if self.json {
            return self.json_line(ev);
        }
        match ev {
            LoopEvent::TextDelta { text } => {
                self.streamed = true;
                self.answer(text);
            }
            LoopEvent::ToolResult {
                call,
                output,
                duration_ms,
                ..
            } => {
                let color = if output.is_error { RED } else { DIM };
                self.note(color, &result_line(call, output.is_error, *duration_ms));
            }
            LoopEvent::Guard { text } => self.note(YELLOW, &format!("! {text}")),
            LoopEvent::Assistant { message } => {
                // Its pieces were not seen (a replay from the ring folds
                // them into the answer): the answer whole.
                if !std::mem::take(&mut self.streamed) {
                    self.answer(&message.text());
                }
                if let Some(err) = &message.error_message {
                    self.note(RED, &format!("model call failed: {err}"));
                }
                self.shown_error = message.error_message.clone();
            }
            LoopEvent::User { message } if self.inputs => {
                self.note(DIM, &format!("> {}", message.content));
            }
            LoopEvent::TurnEnd {
                stop_reason, error, ..
            } => {
                self.end_line();
                let shown = self.shown_error.take();
                match (error, *stop_reason == StopReason::Error) {
                    (Some(e), true) if shown.as_ref() != Some(e) => {
                        self.note(RED, &format!("turn failed: {e}"));
                    }
                    (Some(e), false) => self.note(YELLOW, &format!("turn stopped: {e}")),
                    _ => {}
                }
                if *stop_reason == StopReason::Aborted {
                    self.note(YELLOW, "interrupted");
                }
            }
            LoopEvent::User { .. }
            | LoopEvent::ModelStart { .. }
            | LoopEvent::ThinkingDelta { .. }
            | LoopEvent::ToolCallStart { .. } => {}
            // A kind this terminal does not show yet.
            _ => {}
        }
    }

    /// The cost line after a turn, on stderr.
    pub fn usage(&mut self, usage: &Usage, total_usd: Option<f64>) {
        if self.json {
            return;
        }
        let mut line = format!(
            "{} in / {} out tokens",
            usage.input + usage.cache_read + usage.cache_write,
            usage.output
        );
        if let Some(c) = usage.cost_usd {
            line.push_str(&format!(", ${c:.4}"));
        }
        if let Some(t) = total_usd {
            line.push_str(&format!(" (session ${t:.4})"));
        }
        self.note(DIM, &line);
    }

    /// A line for the human: stderr, or a `notice` line on stdout in json mode.
    pub fn notice(&mut self, level: Level, text: &str) {
        if self.json {
            let text = text.to_string();
            return self.json_line(Outbound::Notice { level, text });
        }
        let color = match level {
            Level::Warning => YELLOW,
            Level::Error => RED,
            // Info, and a level a newer haest-wire adds: plain.
            _ => DIM,
        };
        self.note(color, text);
    }

    /// A question the human answers on the next line: stderr with no newline,
    /// or an `ask` line on stdout in json mode.
    pub fn ask(&mut self, question: &str) {
        if self.json {
            let text = question.to_string();
            return self.json_line(Outbound::Ask { text });
        }
        self.end_line();
        let text = self.paint(YELLOW, question);
        let _ = write!(self.err, "{text}");
        let _ = self.err.flush();
    }

    /// One line on stdout in json mode: `body`, the stream's next frame.
    fn json_line(&mut self, body: impl Serialize) {
        let frame = self.framer.stamp(Path::main(), body);
        let _ = self.out.write_all(encode_line(&frame).as_bytes());
        let _ = self.out.flush();
    }
}

/// `- Read src/a.rs (3 ms)`: a tool call's result, `x` for one that failed.
pub fn result_line(call: &ToolCall, is_error: bool, duration_ms: u64) -> String {
    let mark = if is_error { "x" } else { "-" };
    format!("{mark} {} ({duration_ms} ms)", call_summary(call))
}

/// `Read src/a.rs`, `Bash cargo test`: the tool and the one input that says what it did.
fn call_summary(call: &ToolCall) -> String {
    const KEYS: [&str; 7] = [
        "path",
        "file_path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
    ];
    let arg = KEYS
        .iter()
        .find_map(|k| call.input.get(*k).and_then(Value::as_str))
        .or_else(|| call.input.values().find_map(Value::as_str))
        .unwrap_or("");
    let arg: String = arg.lines().next().unwrap_or("").chars().take(80).collect();
    if arg.is_empty() {
        call.name.clone()
    } else {
        format!("{} {arg}", call.name)
    }
}

#[cfg(test)]
pub(crate) mod tests;
