//! The `haest` binary on a terminal: what a human types and sees when it
//! asks for approval.

#![cfg(unix)]

mod support;

use std::{
    fs::File,
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

/// `haest -p` (and `extra`) in the real workspace with stdin and stdout on
/// the terminal `pty_end`, and stderr there too unless `stderr` says where.
fn haest_on(
    home: &Home,
    extra: &[&str],
    pty_end: std::os::fd::OwnedFd,
    stderr: Option<Stdio>,
) -> Child {
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
    home.command(&[&args, extra].concat())
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
    let mut child = haest_on(&home, &[], end, None);
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

/// With stderr in a file (`2>log`, `2>/dev/null`) a question there would be
/// asked where nobody sees it: haest refuses the command at once, and the
/// file says why.
#[test]
fn with_stderr_in_a_file_an_approval_is_refused_at_once() {
    let (home, _model, victim) = victim_run("stderr-file");
    let log = home.0.join("err.log");
    let (pty, end) = Pty::open();
    let stderr = Stdio::from(File::create(&log).unwrap());
    let mut child = haest_on(&home, &[], end, Some(stderr));
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let waits = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let err = std::fs::read_to_string(&log).unwrap();
    let rows: Vec<&str> = err.lines().collect();
    assert!(
        !waits,
        "haest still waits 20 s later; err.log ends:\n{}\nthe terminal:\n{}",
        rows[rows.len().saturating_sub(3)..].join("\n"),
        pty.screen()
    );
    assert!(victim.exists(), "../victim is gone");
    assert!(!err.contains(ASKED), "asked into the file: {err}");
    assert!(err.contains("stderr"), "the file does not say why: {err}");
}

/// With --json the question is a line on stdout: stderr in a file does not
/// keep haest from asking there.
#[test]
fn with_json_the_question_on_stdout_is_asked_though_stderr_is_a_file() {
    let (home, _model, victim) = victim_run("json-stderr-file");
    let (mut pty, end) = Pty::open();
    let stderr = Stdio::from(File::create(home.0.join("err.log")).unwrap());
    let mut child = haest_on(&home, &["--json"], end, Some(stderr));
    let asked = pty.wait_for(Duration::from_secs(30), |s| s.contains(r#""type":"ask""#));
    assert!(asked, "never asked:\n{}", pty.screen());
    enter_until_exit(&mut child, &mut pty);
    assert!(victim.exists(), "../victim is gone:\n{}", pty.screen());
}
