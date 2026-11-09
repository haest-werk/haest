//! Turning loop events into terminal output.
//!
//! stdout carries the ANSWER only (the model's text), so `kern -p ... > out.md`
//! captures exactly that. Everything about the work -- tool calls, thinking,
//! guard notices, errors, the cost line -- goes to stderr. `--json` replaces all
//! of it with one JSON line per event on stdout.

use std::io::{IsTerminal, Write};

use kern::{LoopEvent, StopReason, ToolCall, Usage};
use serde_json::Value;

/// How events are written.
pub struct Renderer<O: Write, E: Write> {
    out: O,
    err: E,
    json: bool,
    color: bool,
    /// The last thing written to stdout did not end a line.
    open_line: bool,
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
        }
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
        let _ = writeln!(self.err, "{line}");
    }

    pub fn event(&mut self, ev: &LoopEvent) {
        if self.json {
            if let Ok(line) = serde_json::to_string(ev) {
                let _ = writeln!(self.out, "{line}");
                let _ = self.out.flush();
            }
            return;
        }
        match ev {
            LoopEvent::TextDelta { text } => {
                let _ = write!(self.out, "{text}");
                let _ = self.out.flush();
                self.open_line = !text.ends_with('\n');
            }
            LoopEvent::ToolResult {
                call,
                output,
                duration_ms,
                ..
            } => {
                let mark = if output.is_error { "x" } else { "-" };
                let line = format!("{mark} {} ({duration_ms} ms)", call_summary(call));
                let color = if output.is_error { RED } else { DIM };
                self.note(color, &line);
            }
            LoopEvent::Guard { text } => self.note(YELLOW, &format!("! {text}")),
            LoopEvent::Assistant { message } => {
                if let Some(err) = &message.error_message {
                    self.note(RED, &format!("model call failed: {err}"));
                }
            }
            LoopEvent::TurnEnd {
                stop_reason, error, ..
            } => {
                self.end_line();
                if let (Some(e), false) = (error, *stop_reason == StopReason::Error) {
                    self.note(YELLOW, &format!("turn stopped: {e}"));
                }
                if *stop_reason == StopReason::Aborted {
                    self.note(YELLOW, "interrupted");
                }
            }
            LoopEvent::User { .. }
            | LoopEvent::ModelStart { .. }
            | LoopEvent::ThinkingDelta { .. }
            | LoopEvent::ToolCallStart { .. } => {}
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
            let line = serde_json::json!({"type": "notice", "level": level.as_str(), "text": text});
            let _ = writeln!(self.out, "{line}");
            let _ = self.out.flush();
            return;
        }
        let color = match level {
            Level::Info => DIM,
            Level::Warning => YELLOW,
            Level::Error => RED,
        };
        self.note(color, text);
    }

    pub fn info(&mut self, text: &str) {
        self.notice(Level::Info, text);
    }

    /// A question the human answers on the next line: stderr with no newline,
    /// or an `ask` line on stdout in json mode.
    pub fn ask(&mut self, question: &str) {
        if self.json {
            let line = serde_json::json!({"type": "ask", "text": question});
            let _ = writeln!(self.out, "{line}");
            let _ = self.out.flush();
            return;
        }
        self.end_line();
        let text = self.paint(YELLOW, question);
        let _ = write!(self.err, "{text}");
        let _ = self.err.flush();
    }

    pub fn warn(&mut self, text: &str) {
        self.notice(Level::Warning, text);
    }

    pub fn error(&mut self, text: &str) {
        self.notice(Level::Error, text);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

/// `Read src/a.rs`, `Bash cargo test`: the tool and the one input that says what it did.
pub fn call_summary(call: &ToolCall) -> String {
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
mod tests;
