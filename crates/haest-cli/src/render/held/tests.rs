use haest::{LoopEvent, StopReason};

use crate::render::{
    Level, Renderer,
    tests::{read_result, turn_end},
};

/// While the terminal's own question waits, nothing else reaches the screen:
/// not a subagent's many-line description that draws a question of its own,
/// a tool line or the main agent's answer. Once it is answered they follow,
/// in order.
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
        let mut r = Renderer::new(&mut out, &mut err, false, false);
        r.answering();
        r.ask(question);
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
/// before what waited is shown; one typed ahead, answered before it was
/// drawn, holds nothing.
#[test]
fn a_question_given_up_or_answered_ahead_lets_the_lines_go() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false);
        r.answering();
        r.ask("Allow Bash? x\n[y]es / [a]lways / [N]o: ");
        r.notice(Level::Info, "later");
        r.answered(false);
        r.answering();
        r.answered(true);
        r.ask("Allow Bash? y\n[y]es / [a]lways / [N]o: ");
        r.notice(Level::Info, "at once");
    }
    let err = String::from_utf8(err).unwrap();
    assert_eq!(
        err,
        "Allow Bash? x\n[y]es / [a]lways / [N]o: \nlater\nAllow Bash? y\n[y]es / [a]lways / [N]o: at once\n"
    );
}

/// A watcher shows another terminal's question on a line of its own: nothing
/// there answers it, so the next line does not join it.
#[test]
fn a_watcher_ends_the_questions_line() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false).watching();
        r.ask("Allow Bash? x\n[y]es / [a]lways / [N]o: ");
        r.event(&read_result(false));
    }
    let err = String::from_utf8(err).unwrap();
    assert!(err.ends_with("[N]o: \n- Read src/a.rs (3 ms)\n"), "{err:?}");
}
