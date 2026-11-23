//! Turning the session's stream into terminal output.
//!
//! stdout carries the ANSWER only (the model's text), so `haest -p ... > out.md`
//! captures exactly that. Everything about the work -- tool calls, thinking,
//! guard notices, errors, the cost line, subagents -- goes to stderr. `--json`
//! replaces all of it with one JSON line per frame on stdout, each in
//! haest-wire's envelope (`v`, `sid`, `seq`, `ts`, `path`, then the frame's
//! own fields).
//!
//! Nothing it writes to a terminal can act on it: the model's text, a tool
//! call's input, a server's error and a peer's words all go through [`inert`],
//! so none of them can move the cursor, set a scroll region or recolour what
//! follows (an approval question included).

mod held;
mod inert;
mod stream;

use std::{borrow::Cow, io::Write, sync::mpsc::Sender};

use haest::{
    LoopEvent, StopReason, ToolCall, Usage,
    wire::{Framer, Outbound, Path, encode_line},
};
use serde::Serialize;
use serde_json::Value;

pub(crate) use haest::wire::Level;
#[cfg(test)]
pub(crate) use inert::hides;
pub(crate) use inert::{inert, is_a_file};

use self::held::{Held, Question};
use crate::agents::AgentLog;

/// How events are written.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches and line state of one private renderer"
)]
pub(crate) struct Renderer<O: Write, E: Write> {
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
    /// A watcher's view (`haest ctl events`): each input that enters the
    /// conversation is written (at the terminal the human typed it).
    watching: bool,
    /// The answer is shown [`inert`] too: stdout is not a regular file, so it
    /// may end on this terminal (a pipe into `tee`). Into a file it goes byte
    /// for byte.
    inert_answer: bool,
    /// The terminal's own question, from when it goes into the stream until
    /// it is answered or given up (the prompter brackets the read).
    question: Option<Question>,
    /// The stream this terminal shows has ended: a question asked now is
    /// never drawn.
    ended: bool,
    /// What came after the question while it waited, shown once it is
    /// answered: the question stays the last thing on the screen.
    held: Option<Held>,
}

const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

impl<O: Write, E: Write> Renderer<O, E> {
    pub(crate) fn new(out: O, err: E, json: bool, color: bool) -> Self {
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
            watching: false,
            inert_answer: false,
            question: None,
            ended: false,
            held: None,
        }
    }

    /// stdout may end on a terminal: the answer is shown [`inert`], as stderr
    /// is.
    pub(crate) fn inert_answer(mut self) -> Self {
        self.inert_answer = true;
        self
    }

    /// A watcher's view: `haest ctl events` watches a conversation someone
    /// else drives, so each input that enters it is written, and its
    /// questions are answered elsewhere.
    pub(crate) fn watching(mut self) -> Self {
        self.watching = true;
        self
    }

    /// The prompt opened (Some: its printer) or closed (None).
    pub(crate) fn set_above(&mut self, above: Option<Sender<String>>) {
        self.above = above;
    }

    /// The REPL prompt is open: the human is (or may be) typing.
    pub(crate) fn prompting(&self) -> bool {
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
            self.put_out("\n");
            self.open_line = false;
        }
    }

    fn note(&mut self, color: &str, text: &str) {
        self.end_line();
        let line = self.paint(color, &inert(text));
        self.put_line(line);
    }

    /// Answer text, to stdout as it comes.
    fn answer(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let text = if self.inert_answer {
            inert(text)
        } else {
            Cow::Borrowed(text)
        };
        self.put_out(&text);
        self.open_line = !text.ends_with('\n');
    }

    pub(crate) fn event(&mut self, ev: &LoopEvent) {
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
                // An answer is whole: the next one (a turn a hook kept going)
                // starts on its own line.
                self.end_line();
                if let Some(err) = &message.error_message {
                    self.note(RED, &format!("model call failed: {err}"));
                }
                self.shown_error = message.error_message.clone();
            }
            LoopEvent::User { message } if self.watching => {
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
    pub(crate) fn usage(&mut self, usage: &Usage, total_usd: Option<f64>) {
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
    pub(crate) fn notice(&mut self, level: Level, text: &str) {
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

    /// One line on stdout in json mode: `body`, the stream's next frame.
    fn json_line(&mut self, body: impl Serialize) {
        let frame = self.framer.stamp(Path::main(), body);
        let _ = self.out.write_all(encode_line(&frame).as_bytes());
        let _ = self.out.flush();
    }
}

/// `- Read src/a.rs (3 ms)`: a tool call's result, `x` for one that failed.
pub(crate) fn result_line(call: &ToolCall, is_error: bool, duration_ms: u64) -> String {
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
