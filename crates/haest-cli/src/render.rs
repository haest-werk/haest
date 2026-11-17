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

use std::{
    borrow::Cow,
    collections::VecDeque,
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

pub(crate) use haest::wire::Level;

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
    /// conversation is written (at the terminal the human typed it), and
    /// another peer answers the questions.
    watching: bool,
    /// The answer is shown [`inert`] too: stdout is not a regular file, so it
    /// may end on this terminal (a pipe into `tee`). Into a file it goes byte
    /// for byte.
    inert_answer: bool,
    /// The terminal's own question is about to be asked or waits for its
    /// answer (the prompter brackets the read).
    answering: bool,
    /// What came after the question while it waited, in order, shown once it
    /// is answered: the question stays the last thing on the screen.
    held: Option<VecDeque<Held>>,
    /// Held lines let go of because too many waited.
    held_dropped: usize,
}

/// A piece of output that waits for the question's answer.
enum Held {
    /// Answer text for stdout (consecutive pieces in one).
    Out(String),
    /// A line for stderr.
    Line(String),
}

/// At most this many pieces wait for a question's answer; past it the oldest
/// is let go of, and counted.
const MAX_HELD: usize = 2000;

fn hold(held: &mut VecDeque<Held>, dropped: &mut usize, piece: Held) {
    if held.len() == MAX_HELD {
        held.pop_front();
        *dropped += 1;
    }
    held.push_back(piece);
}

const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

impl Renderer<std::io::Stdout, std::io::Stderr> {
    pub(crate) fn terminal(json: bool) -> Self {
        let color = std::io::stderr().is_terminal();
        let r = Renderer::new(std::io::stdout(), std::io::stderr(), json, color);
        // Byte for byte only into a file: a pipe can end on this terminal
        // (`| tee out.md`), right where a question is asked.
        if is_a_file(&std::io::stdout()) {
            r
        } else {
            r.inert_answer()
        }
    }
}

/// `out` is a regular file (`> out.md`), not a terminal, a pipe or a socket.
#[cfg(unix)]
pub(crate) fn is_a_file(out: &impl std::os::fd::AsFd) -> bool {
    out.as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .and_then(|f| f.metadata())
        .is_ok_and(|m| m.is_file())
}

/// `out` is a regular file (`> out.md`), not a console, a pipe or a socket.
#[cfg(windows)]
pub(crate) fn is_a_file(out: &impl std::os::windows::io::AsHandle) -> bool {
    out.as_handle()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .and_then(|f| f.metadata())
        .is_ok_and(|m| m.is_file())
}

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
            answering: false,
            held: None,
            held_dropped: 0,
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

    /// One frame of the session's stream: in json mode its line as it is;
    /// otherwise what the human reads of it. The cost line comes after a
    /// turn of the conversation's own, with the session's total from the
    /// status that follows it.
    pub(crate) fn frame(&mut self, frame: &Frame) {
        if self.json {
            let _ = self.out.write_all(encode_line(frame).as_bytes());
            let _ = self.out.flush();
            return;
        }
        let subagent = frame.path.segments().nth(1);
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

    /// `text` to stdout, or held while the question waits.
    fn put_out(&mut self, text: &str) {
        if let Some(held) = &mut self.held {
            match held.back_mut() {
                Some(Held::Out(run)) => run.push_str(text),
                _ => hold(held, &mut self.held_dropped, Held::Out(text.to_owned())),
            }
            return;
        }
        let _ = write!(self.out, "{text}");
        let _ = self.out.flush();
    }

    /// One line to stderr (above the REPL prompt while it is open), or held
    /// while the question waits.
    fn put_line(&mut self, line: String) {
        if let Some(held) = &mut self.held {
            return hold(held, &mut self.held_dropped, Held::Line(line));
        }
        match &self.above {
            Some(above) if above.send(line.clone()).is_ok() => {}
            _ => {
                let _ = writeln!(self.err, "{line}");
            }
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

    /// The terminal's own question is about to be asked: from when it is
    /// drawn until [`answered`](Self::answered), every other line waits, so
    /// the question stays the last thing on the screen.
    pub(crate) fn answering(&mut self) {
        self.answering = true;
    }

    /// The question was answered (`enter`: the human ended its line) or given
    /// up: what waited is shown, in order.
    pub(crate) fn answered(&mut self, enter: bool) {
        self.answering = false;
        let Some(held) = self.held.take() else {
            return;
        };
        if !enter {
            let _ = writeln!(self.err);
        }
        for h in held {
            match h {
                Held::Out(text) => self.put_out(&text),
                Held::Line(line) => self.put_line(line),
            }
        }
        let dropped = std::mem::take(&mut self.held_dropped);
        if dropped > 0 {
            let lines = format!("({dropped} lines not shown while haest asked)");
            self.note(DIM, &lines);
        }
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

    /// A question the human answers on the next line: stderr with no newline,
    /// or an `ask` line on stdout in json mode. A watcher, which another peer
    /// answers for, ends its line.
    pub(crate) fn ask(&mut self, question: &str) {
        if self.json {
            let text = question.to_string();
            return self.json_line(Outbound::Ask { text });
        }
        self.end_line();
        let text = self.paint(YELLOW, &inert(question));
        if self.watching {
            let _ = writeln!(self.err, "{text}");
        } else {
            let _ = write!(self.err, "{text}");
        }
        let _ = self.err.flush();
        if self.answering && !self.watching {
            self.held.get_or_insert_with(VecDeque::new);
        }
    }

    /// One line on stdout in json mode: `body`, the stream's next frame.
    fn json_line(&mut self, body: impl Serialize) {
        let frame = self.framer.stamp(Path::main(), body);
        let _ = self.out.write_all(encode_line(&frame).as_bytes());
        let _ = self.out.flush();
    }
}

/// `text` with nothing in it a terminal acts on: every control character but
/// a newline and a tab, and every character that reorders or hides text, as
/// its escape (`\u{1b}`, `\r`, `\u{202e}`). An escape is printable, so a
/// second pass changes nothing.
pub(crate) fn inert(text: &str) -> Cow<'_, str> {
    let acts = |c: char| hides(c) || (c.is_control() && c != '\n' && c != '\t');
    if !text.chars().any(acts) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if hides(c) {
            out.extend(c.escape_unicode());
        } else if acts(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Bidi controls and zero-width characters: they reorder text or hide it.
pub(crate) fn hides(c: char) -> bool {
    matches!(c,
        '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
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
