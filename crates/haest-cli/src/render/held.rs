//! The terminal's own question, and what waits for its answer: from when the
//! question is drawn until it is answered (or given up), every other line and
//! the answer text are held, so the question stays the last thing on the
//! screen; then they follow it, in order.

use std::{collections::VecDeque, io::Write};

use super::{DIM, Renderer, YELLOW, inert};

/// A piece of output that waits for the question's answer.
pub(super) enum Held {
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

impl<O: Write, E: Write> Renderer<O, E> {
    /// `text` to stdout, or held while the question waits.
    pub(super) fn put_out(&mut self, text: &str) {
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
    pub(super) fn put_line(&mut self, line: String) {
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

    /// A question the human answers on the next line: stderr with no newline,
    /// or an `ask` line on stdout in json mode. A watcher, which another peer
    /// answers for, ends its line.
    pub(crate) fn ask(&mut self, question: &str) {
        if self.json {
            let text = question.to_string();
            return self.json_line(haest::wire::Outbound::Ask { text });
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
}

#[cfg(test)]
mod tests;
