//! The terminal's own question, and what waits for its answer: from when the
//! question is drawn until it is answered (or given up), every other line and
//! the answer text are held, so the question stays the last thing on the
//! screen; then they follow it, in order. Its prompter is told when it is
//! drawn, and reads nothing typed before.

use std::{collections::VecDeque, io::Write};

use haest::{
    session::{Body, Frame},
    wire::Outbound,
};
use tokio::sync::oneshot;

use super::{DIM, Renderer, YELLOW, inert};

/// The terminal's own question, from when it goes into the stream until it
/// is answered or given up.
pub(super) struct Question {
    /// Its frame, as the stream has it: written (json) or drawn from here if
    /// the terminal loses it.
    frame: Frame,
    /// Told once it is drawn; None from then on.
    drawn: Option<oneshot::Sender<()>>,
}

/// What `frame`, an `ask`, asks.
fn asks(frame: &Frame) -> &str {
    match &frame.body {
        Body::Wire(Outbound::Ask { text, .. }) => text,
        // Only an ask is a question (`TermUi::ask` makes it).
        _ => "",
    }
}

/// What came after the question while it waited, in order, shown once it is
/// answered.
#[derive(Default)]
pub(super) struct Held {
    pieces: VecDeque<Piece>,
    /// How many of the pieces are lines.
    lines: usize,
    /// Lines let go of because too many waited.
    dropped: usize,
}

/// A piece of output that waits for the question's answer.
enum Piece {
    /// Answer text for stdout (consecutive pieces in one).
    Out(String),
    /// A line for stderr (a notice may take several rows).
    Line(String),
}

/// At most this many lines wait for a question's answer; past it the oldest
/// is let go of, and counted. Answer text is never let go of: it is the
/// run's output, and the file it goes into keeps all of it.
const MAX_HELD: usize = 2000;

impl Held {
    /// Answer text, after what waits.
    fn out(&mut self, text: &str) {
        match self.pieces.back_mut() {
            Some(Piece::Out(run)) => run.push_str(text),
            _ => self.pieces.push_back(Piece::Out(text.to_owned())),
        }
    }

    /// A line, after what waits; past [`MAX_HELD`] lines the oldest goes.
    fn line(&mut self, line: String) {
        if self.lines >= MAX_HELD {
            self.let_the_oldest_line_go();
        }
        self.pieces.push_back(Piece::Line(line));
        self.lines += 1;
    }

    /// The oldest line goes, counted as the rows it takes, and the answer
    /// text on either side of it is one piece again.
    fn let_the_oldest_line_go(&mut self) {
        let oldest = self.pieces.iter().position(|p| matches!(p, Piece::Line(_)));
        let Some(i) = oldest else {
            return;
        };
        if let Some(Piece::Line(line)) = self.pieces.remove(i) {
            self.lines -= 1;
            self.dropped += line.split('\n').count();
        }
        let before = i.checked_sub(1).and_then(|b| self.pieces.get(b));
        let joins = matches!(
            (before, self.pieces.get(i)),
            (Some(Piece::Out(_)), Some(Piece::Out(_)))
        );
        if joins
            && let Some(Piece::Out(after)) = self.pieces.remove(i)
            && let Some(Piece::Out(before)) = self.pieces.get_mut(i - 1)
        {
            before.push_str(&after);
        }
    }
}

impl<O: Write, E: Write> Renderer<O, E> {
    /// `text` to stdout, or held while the question waits. Into a regular
    /// file it never reaches the screen, and is not held.
    pub(super) fn put_out(&mut self, text: &str) {
        if self.inert_answer
            && let Some(held) = &mut self.held
        {
            return held.out(text);
        }
        let _ = write!(self.out, "{text}");
        let _ = self.out.flush();
    }

    /// One line to stderr (above the REPL prompt while it is open), or held
    /// while the question waits.
    pub(super) fn put_line(&mut self, line: String) {
        if let Some(held) = &mut self.held {
            return held.line(line);
        }
        match &self.above {
            Some(above) if above.send(line.clone()).is_ok() => {}
            _ => {
                let _ = writeln!(self.err, "{line}");
            }
        }
    }

    /// The terminal's own question went into the stream as `frame`, an
    /// `ask`: from when it is drawn until [`answered`](Self::answered), every
    /// other line waits, so the question stays the last thing on the screen.
    /// `drawn` is told once it is on the screen, and drops untold if it never
    /// will be (the stream has ended).
    pub(crate) fn answering(&mut self, frame: Frame, drawn: oneshot::Sender<()>) {
        if !self.ended {
            let drawn = Some(drawn);
            self.question = Some(Question { frame, drawn });
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
            .is_some_and(|q| q.frame.seq == seq && q.drawn.is_some())
        {
            self.draw_question(text);
        } else {
            self.ask(text);
        }
    }

    /// Frame `seq` comes after the terminal's own question, which was not
    /// drawn: its frame was lost (the terminal fell behind the stream), so it
    /// is drawn now, before what came after it; in json mode its line is the
    /// stream's frame, as it would have been.
    pub(super) fn after_lost_question(&mut self, seq: u64) {
        let Some(q) = &self.question else {
            return;
        };
        if q.drawn.is_none() || seq <= q.frame.seq {
            return;
        }
        // Copied out: writing it borrows the renderer.
        let frame = q.frame.clone();
        if self.json {
            self.json_frame(&frame);
        } else {
            self.draw_question(asks(&frame));
        }
    }

    /// The terminal's own question on stderr, its line open for the answer;
    /// from now on every other line waits.
    fn draw_question(&mut self, text: &str) {
        self.end_line();
        let text = self.paint(YELLOW, &inert(text));
        let _ = write!(self.err, "{text}");
        let _ = self.err.flush();
        self.held.get_or_insert_with(Held::default);
        self.drew_question();
    }

    /// Json mode wrote frame `seq` as it is: when it is the terminal's own
    /// question, its prompter is told.
    pub(super) fn wrote(&mut self, seq: u64) {
        if self.question.as_ref().is_some_and(|q| q.frame.seq == seq) {
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
        for piece in held.pieces {
            match piece {
                Piece::Out(text) => self.put_out(&text),
                Piece::Line(line) => self.put_line(line),
            }
        }
        let lines = match held.dropped {
            0 => return,
            1 => "1 line".to_owned(),
            n => format!("{n} lines"),
        };
        self.note(DIM, &format!("({lines} not shown while haest asked)"));
    }

    /// An approval refused, since nobody could answer it here, said on
    /// stderr. In text mode its warning in the stream shows there already;
    /// with --json the stream is stdout (a file, or a program's pipe), so
    /// this is where the human at the terminal reads why.
    pub(crate) fn refused(&mut self, why: &str) {
        if self.json {
            self.note(YELLOW, why);
        }
    }

    /// A question nothing on this terminal reads the answer to (a watcher's
    /// view, or one given up before it was drawn): a line of its own on
    /// stderr, so the next line does not join it, or an `ask` line on stdout
    /// in json mode.
    pub(super) fn ask(&mut self, question: &str) {
        if self.json {
            let text = question.to_string();
            let approval = None;
            return self.json_line(haest::wire::Outbound::Ask { text, approval });
        }
        self.note(YELLOW, question);
    }
}

#[cfg(test)]
mod tests;
