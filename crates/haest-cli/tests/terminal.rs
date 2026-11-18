//! The `haest` binary on a terminal: what a human types and sees when it
//! asks for approval.

#![cfg(unix)]

mod support;

use std::{
    path::PathBuf,
    process::{Child, Stdio},
    time::{Duration, Instant},
};

use support::{FakeModel, Home, LOCAL_KEY, Reply, bash, pty::Pty};

/// How the question ends, right where the answer goes.
const ASKED: &str = "[y]es / [a]lways / [N]o: ";

/// A project whose neighbour `victim` the model wants gone: it waits 2 s
/// (`sleep 2`, which needs no approval), then asks to run
/// `rm -rf ../victim`, then says it is done.
fn victim_run(name: &str) -> (Home, FakeModel, PathBuf) {
    let home = Home::new(name);
    let victim = home.0.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "x").unwrap();
    let model = FakeModel::new([
        bash("sleep 2"),
        bash("rm -rf ../victim"),
        Reply::Text("done".into()),
    ]);
    home.serve(&model);
    (home, model, victim)
}

/// `haest -p` in the real workspace with stdin and stdout on `pty`, and
/// stderr there too unless `stderr` says where.
fn haest_on(home: &Home, pty_end: std::os::fd::OwnedFd, stderr: Option<Stdio>) -> Child {
    let args = [
        "-p",
        "clean up",
        "-m",
        "local:m",
        "--workspace",
        "real",
        "--tools",
        "bash",
    ];
    let stderr = stderr.unwrap_or_else(|| Stdio::from(pty_end.try_clone().unwrap()));
    home.command(&args)
        .env(LOCAL_KEY.0, LOCAL_KEY.1)
        .stdin(Stdio::from(pty_end.try_clone().unwrap()))
        .stdout(Stdio::from(pty_end))
        .stderr(stderr)
        .spawn()
        .unwrap()
}

/// Waits up to 30 s for `child` to exit; while it runs, Enter is pressed
/// every half second (a human who means No). Enter is pressed again because
/// one typed in the instant the question is drawn may be dropped with what
/// was typed before it.
fn enter_until_exit(child: &mut Child, pty: &mut Pty) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("haest still runs 30 s later:\n{}", pty.screen());
        }
        pty.type_keys("\n");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Keys typed before the question is drawn, then Enter once it is, answer
/// it only as that Enter: No.
fn typed_ahead_is_not_the_answer(name: &str, typed: &str) {
    let (home, _model, victim) = victim_run(name);
    let (mut pty, end) = Pty::open();
    let mut child = haest_on(&home, end, None);
    // At the model's own text, say, or at an ask given up before this one.
    pty.type_keys(typed);
    let asked = pty.wait_for(Duration::from_secs(30), |s| s.contains(ASKED));
    assert!(asked, "never asked:\n{}", pty.screen());
    enter_until_exit(&mut child, &mut pty);
    assert!(
        victim.exists(),
        "{typed:?}, typed before the question was drawn, answered it: ../victim is gone\n{}",
        pty.screen()
    );
}

/// A whole line typed before the question exists (at a question the model's
/// text drew) does not answer the real one.
#[test]
fn a_line_typed_before_the_question_does_not_answer_it() {
    typed_ahead_is_not_the_answer("typed-line", "y\n");
}

/// A key typed before the question exists (at an ask that was then given
/// up), and Enter once the real one shows, is an empty answer: No.
#[test]
fn a_key_typed_before_the_question_does_not_answer_it() {
    typed_ahead_is_not_the_answer("typed-key", "y");
}
