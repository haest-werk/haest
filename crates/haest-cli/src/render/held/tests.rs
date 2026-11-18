use haest::{LoopEvent, StopReason, wire::Outbound};
use tokio::sync::oneshot::{self, error::TryRecvError};

use crate::render::{
    Level, Renderer,
    tests::{read_result, turn_end, wire_frame},
};

/// Frame `seq` of the stream: the question `text`.
fn ask_frame(seq: u64, text: &str) -> haest::session::Frame {
    let text = text.into();
    wire_frame(seq, Outbound::Ask { text })
}

/// Frame `seq` of the stream: a line for the human.
fn notice_frame(seq: u64, text: &str) -> haest::session::Frame {
    let (level, text) = (Level::Info, text.into());
    wire_frame(seq, Outbound::Notice { level, text })
}

/// `r` asks the terminal's own question `text` as frame `seq`: what tells
/// its prompter it was drawn.
fn asking<O: std::io::Write, E: std::io::Write>(
    r: &mut Renderer<O, E>,
    seq: u64,
    text: &str,
) -> oneshot::Receiver<()> {
    let (tell, drawn) = oneshot::channel();
    r.answering(ask_frame(seq, text), tell);
    drawn
}

/// While the terminal's own question waits, nothing else reaches the screen:
/// not a subagent's many-line description that draws a question of its own,
/// a tool line or the main agent's answer (stdout may be the screen too).
/// Once it is answered they follow, in order.
#[test]
fn nothing_lands_below_the_question_until_it_is_answered() {
    let question = "  touch PWNED\n  rm -rf ./build\nAllow Bash? deletes (2 lines above)\n[y]es / [a]lways / [N]o: ";
    let forged = format!(
        "{}  rm -rf ./build\nAllow Bash? deletes\n[y]es / [a]lways / [N]o: ",
        "\n".repeat(30)
    );
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (out_then, err_then);
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false).inert_answer();
        let mut drawn = asking(&mut r, 1, question);
        r.frame(&ask_frame(1, question));
        assert_eq!(drawn.try_recv(), Ok(()), "its prompter is told");
        r.notice(Level::Info, &forged);
        r.event(&read_result(false));
        r.event(&LoopEvent::TextDelta {
            text: "meanwhile".into(),
        });
        r.event(&turn_end(StopReason::EndTurn, None));
        (out_then, err_then) = (r.out.len(), r.err.len());
        r.answered(true);
    }
    assert_eq!(out_then, 0, "nothing on stdout while it waits");
    assert_eq!(err_then, question.len(), "the question is the last thing");
    let (out, err) = (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    );
    assert_eq!(out, "meanwhile\n");
    let after = &err[question.len()..];
    assert!(after.starts_with(&forged), "{after:?}");
    assert!(after.ends_with("- Read src/a.rs (3 ms)\n"), "{after:?}");
}

/// A question given up (the read dropped, or stdin closed) ends its line
/// before what waited is shown. One given up before it was drawn holds
/// nothing, and is drawn on a line of its own when its frame comes: nothing
/// reads its answer.
#[test]
fn a_question_given_up_lets_the_lines_go() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false);
        let _drawn = asking(&mut r, 1, "Allow Bash? x\n[y]es / [a]lways / [N]o: ");
        r.frame(&ask_frame(1, "Allow Bash? x\n[y]es / [a]lways / [N]o: "));
        r.notice(Level::Info, "later");
        r.answered(false);
        let _drawn = asking(&mut r, 2, "Allow Bash? y\n[y]es / [a]lways / [N]o: ");
        r.answered(false);
        r.frame(&ask_frame(2, "Allow Bash? y\n[y]es / [a]lways / [N]o: "));
        r.notice(Level::Info, "at once");
    }
    let err = String::from_utf8(err).unwrap();
    assert_eq!(
        err,
        "Allow Bash? x\n[y]es / [a]lways / [N]o: \nlater\nAllow Bash? y\n[y]es / [a]lways / [N]o: \nat once\n"
    );
}

/// The prompter is told only when its own question is on the screen: not
/// when an earlier question's frame is drawn late (an ask given up before
/// its frame was drawn), which takes a line of its own.
#[test]
fn the_prompter_is_told_when_its_own_question_is_drawn() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false);
        let mut drawn = asking(&mut r, 5, "Allow Bash? new\n[y]es / [a]lways / [N]o: ");
        r.frame(&ask_frame(4, "Allow Bash? old\n[y]es / [a]lways / [N]o: "));
        assert_eq!(drawn.try_recv(), Err(TryRecvError::Empty), "not yet");
        r.frame(&ask_frame(5, "Allow Bash? new\n[y]es / [a]lways / [N]o: "));
        assert_eq!(drawn.try_recv(), Ok(()));
    }
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.ends_with("[N]o: \nAllow Bash? new\n[y]es / [a]lways / [N]o: "),
        "{err:?}"
    );

    // In json mode, once its line is written.
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut r = Renderer::new(&mut out, &mut err, true, false);
    let mut drawn = asking(&mut r, 2, "Allow Bash?");
    r.frame(&notice_frame(1, "before"));
    assert_eq!(drawn.try_recv(), Err(TryRecvError::Empty), "not yet");
    r.frame(&ask_frame(2, "Allow Bash?"));
    assert_eq!(drawn.try_recv(), Ok(()));
}

/// A question whose frame the terminal lost (it fell behind the stream) is
/// drawn before what came after it, and its prompter is told.
#[test]
fn a_question_whose_frame_was_lost_is_drawn_before_what_follows() {
    let question = "Allow Bash? x\n[y]es / [a]lways / [N]o: ";
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false);
        let mut drawn = asking(&mut r, 5, question);
        let lost = Outbound::Gap {
            after: 3,
            missed: Some(4),
        };
        r.frame(&wire_frame(3, lost));
        assert_eq!(drawn.try_recv(), Err(TryRecvError::Empty), "not yet");
        r.frame(&notice_frame(7, "after it"));
        assert_eq!(drawn.try_recv(), Ok(()));
        r.answered(true);
    }
    let err = String::from_utf8(err).unwrap();
    assert!(err.ends_with(&format!("{question}after it\n")), "{err:?}");
}

/// In json mode a question whose frame the terminal lost is written as the
/// stream's frame: its sid and seq, so a reader that checks the one or
/// resumes by the other reads it right.
#[test]
fn in_json_a_question_whose_frame_was_lost_is_the_streams_frame() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, true, false);
        let mut drawn = asking(&mut r, 5, "Allow Bash?");
        let lost = Outbound::Gap {
            after: 3,
            missed: Some(4),
        };
        r.frame(&wire_frame(3, lost));
        r.frame(&notice_frame(7, "after it"));
        assert_eq!(drawn.try_recv(), Ok(()));
    }
    let out = String::from_utf8(out).unwrap();
    let lines: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["gap", "ask", "notice"], "{out}");
    let ask = &lines[1];
    assert_eq!(
        (ask["sid"].as_str(), ask["seq"].as_u64()),
        (Some("s"), Some(5)),
        "{out}"
    );
    assert_eq!(ask["text"], "Allow Bash?");
}

/// A question asked once the stream has ended is never drawn, nor is one
/// that waited for its frame: their prompters hear so.
#[test]
fn a_question_the_ended_stream_never_draws_tells_its_prompter() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut r = Renderer::new(&mut out, &mut err, false, false);
    let mut waited = asking(&mut r, 1, "Allow Bash?");
    r.ended();
    assert_eq!(waited.try_recv(), Err(TryRecvError::Closed));
    let mut late = asking(&mut r, 2, "Allow Bash?");
    assert_eq!(late.try_recv(), Err(TryRecvError::Closed));
}

/// The number in the last line of `text` (a note's count).
fn count_in_last_line(text: &str) -> Option<usize> {
    let last = text.lines().last()?;
    last.split(|c: char| !c.is_ascii_digit())
        .find(|n| !n.is_empty())?
        .parse()
        .ok()
}

/// Past the cap the oldest held LINES are let go of, counted as the lines
/// they were; the answer's text never is, so `-p ... > out.md` keeps all of
/// it.
#[test]
fn held_answer_text_is_never_let_go_of() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false).inert_answer();
        let _drawn = asking(&mut r, 1, "Allow Bash?");
        r.frame(&ask_frame(1, "Allow Bash?"));
        r.event(&LoopEvent::TextDelta {
            text: "HEAD\n".into(),
        });
        r.notice(Level::Info, "oldest a\noldest b\noldest c");
        r.event(&LoopEvent::TextDelta {
            text: "MIDDLE\n".into(),
        });
        for n in 0..2000 {
            r.notice(Level::Info, &format!("line {n}"));
        }
        // Given up: the question's line is ended before what waited.
        r.answered(false);
    }
    let (out, err) = (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    );
    assert_eq!(out, "HEAD\nMIDDLE\n", "the answer's start was let go of");
    assert!(!err.contains("oldest"), "the oldest line stays: {err}");
    assert!(err.contains("\nline 0\n") && err.contains("\nline 1999\n"));
    let note = err.lines().last();
    assert_eq!(count_in_last_line(&err), Some(3), "{note:?}");
}

/// Answer text into a regular file never reaches the screen, so it is not
/// held while the question waits.
#[test]
fn answer_text_into_a_file_is_not_held() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let out_then;
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false);
        let _drawn = asking(&mut r, 1, "Allow Bash?");
        r.frame(&ask_frame(1, "Allow Bash?"));
        r.event(&LoopEvent::TextDelta {
            text: "meanwhile".into(),
        });
        out_then = r.out.len();
        r.answered(true);
    }
    assert_eq!(
        out_then,
        "meanwhile".len(),
        "held, though it goes to a file"
    );
}

/// A watcher shows another terminal's question on a line of its own: nothing
/// there answers it, so the next line does not join it.
#[test]
fn a_watcher_ends_the_questions_line() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false).watching();
        r.frame(&ask_frame(1, "Allow Bash? x\n[y]es / [a]lways / [N]o: "));
        r.event(&read_result(false));
    }
    let err = String::from_utf8(err).unwrap();
    assert!(err.ends_with("[N]o: \n- Read src/a.rs (3 ms)\n"), "{err:?}");
}
