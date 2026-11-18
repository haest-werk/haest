//! The terminal's own question, and what waits for its answer: from when the
//! question is drawn until it is answered (or given up), every other line and
//! the answer text are held, so the question stays the last thing on the
//! screen; then they follow it, in order. Its prompter is told when it is
//! drawn, and reads nothing typed before.

use std::{collections::VecDeque, io::Write};

use tokio::sync::oneshot;

use super::{DIM, Renderer, YELLOW, inert};

/// The terminal's own question, from when it goes into the stream until it
/// is answered or given up.
pub(super) struct Question {
    /// Its frame's seq.
    seq: u64,
    /// What it asks: drawn from here if its frame is lost.
    text: String,
    /// Told once it is drawn; None from then on.
    drawn: Option<oneshot::Sender<()>>,
}

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

    /// The terminal's own question went into the stream as frame `seq`: from
    /// when it is drawn until [`answered`](Self::answered), every other line
    /// waits, so the question stays the last thing on the screen. `drawn` is
    /// told once it is on the screen, and drops untold if it never will be
    /// (the stream has ended).
    pub(crate) fn answering(&mut self, seq: u64, text: String, drawn: oneshot::Sender<()>) {
        if !self.ended {
            let drawn = Some(drawn);
            self.question = Some(Question { seq, text, drawn });
        }
    }

    /// The stream this terminal shows has ended: a question not drawn yet
    /// never will be.
    pub(crate) fn ended(&mut self) {
        self.ended = true;
        if self.question.as_ref().is_some_and(|q| q.drawn.is_some()) {
            self.question = None;
        }
    }

    /// Frame `seq` asks `text`: the terminal's own question is drawn with its
    /// line open for the answer; any other (a watcher's, or one given up
    /// before it was drawn) is a line like the rest.
    pub(super) fn asked(&mut self, seq: u64, text: &str) {
        if self
            .question
            .as_ref()
            .is_some_and(|q| q.seq == seq && q.drawn.is_some())
        {
            self.draw_question(text);
        } else {
            self.ask(text);
        }
    }

    /// Frame `seq` comes after the terminal's own question, which was not
    /// drawn: its frame was lost (the terminal fell behind the stream), so it
    /// is drawn now, before what came after it.
    pub(super) fn after_lost_question(&mut self, seq: u64) {
        let Some(q) = self.question.as_mut() else {
            return;
        };
        if q.drawn.is_none() || seq <= q.seq {
            return;
        }
        let text = std::mem::take(&mut q.text);
        if self.json {
            self.ask(&text);
            self.drew_question();
        } else {
            self.draw_question(&text);
        }
    }

    /// The terminal's own question on stderr, its line open for the answer;
    /// from now on every other line waits.
    fn draw_question(&mut self, text: &str) {
        self.end_line();
        let text = self.paint(YELLOW, &inert(text));
        let _ = write!(self.err, "{text}");
        let _ = self.err.flush();
        self.held.get_or_insert_with(VecDeque::new);
        self.drew_question();
    }

    /// Json mode wrote frame `seq` as it is: when it is the terminal's own
    /// question, its prompter is told.
    pub(super) fn wrote(&mut self, seq: u64) {
        if self.question.as_ref().is_some_and(|q| q.seq == seq) {
            self.drew_question();
        }
    }

    /// The terminal's own question is on the screen: its prompter is told,
    /// so what is typed from now on answers it.
    fn drew_question(&mut self) {
        if let Some(drawn) = self.question.as_mut().and_then(|q| q.drawn.take()) {
            let _ = drawn.send(());
        }
    }

    /// The question was answered (`enter`: the human ended its line) or given
    /// up: what waited is shown, in order.
    pub(crate) fn answered(&mut self, enter: bool) {
        self.question = None;
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

    /// A question nothing on this terminal reads the answer to (a watcher's
    /// view, or one given up before it was drawn): a line of its own on
    /// stderr, so the next line does not join it, or an `ask` line on stdout
    /// in json mode.
    pub(super) fn ask(&mut self, question: &str) {
        if self.json {
            let text = question.to_string();
            return self.json_line(haest::wire::Outbound::Ask { text });
        }
        self.note(YELLOW, question);
    }
}

#[cfg(test)]
mod tests;
