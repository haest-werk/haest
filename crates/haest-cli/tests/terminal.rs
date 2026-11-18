//! The `haest` binary on a terminal: what a human types and sees when it
//! asks for approval.

mod support;

use std::{
    fs::File,
    os::fd::OwnedFd,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::json;
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

/// `haest -p` with its prompt: one answer, then exit.
const PRINT: [&str; 2] = ["-p", "clean up"];

/// `haest` with `args` (say [`PRINT`]) in the real workspace, its stdin,
/// stdout and stderr on the terminal `pty_end` (set one elsewhere before it
/// is spawned).
fn haest_on(home: &Home, args: &[&str], pty_end: &OwnedFd) -> Command {
    let model = ["-m", "local:m", "--workspace", "real", "--tools", "bash"];
    let on_the_terminal = || Stdio::from(pty_end.try_clone().unwrap());
    let mut cmd = home.command(&[&model, args].concat());
    cmd.env(LOCAL_KEY.0, LOCAL_KEY.1)
        .stdin(on_the_terminal())
        .stdout(on_the_terminal())
        .stderr(on_the_terminal());
    cmd
}

/// Waits up to `timeout` for `child` to exit: true when it has.
fn exits_within(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
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
    let mut child = haest_on(&home, &PRINT, &end).spawn().unwrap();
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
    let mut child = haest_on(&home, &PRINT, &end)
        .stderr(File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let waits = !exits_within(&mut child, Duration::from_secs(20));
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
    let mut child = haest_on(&home, &[&PRINT[..], &["--json"]].concat(), &end)
        .stderr(File::create(home.0.join("err.log")).unwrap())
        .spawn()
        .unwrap();
    let asked = pty.wait_for(Duration::from_secs(30), |s| s.contains(r#""type":"ask""#));
    assert!(asked, "never asked:\n{}", pty.screen());
    enter_until_exit(&mut child, &mut pty);
    assert!(victim.exists(), "../victim is gone:\n{}", pty.screen());
}

/// With --json and stdout in a file (`> out.jsonl`) the question would be a
/// line nobody sees: haest refuses the command at once, says why on stderr,
/// and the stream in the file says it too.
#[test]
fn with_json_and_stdout_in_a_file_an_approval_is_refused_at_once() {
    let (home, _model, victim) = victim_run("json-stdout-file");
    let out = home.0.join("out.jsonl");
    let (pty, end) = Pty::open();
    let mut child = haest_on(&home, &[&PRINT[..], &["--json"]].concat(), &end)
        .stdout(File::create(&out).unwrap())
        .spawn()
        .unwrap();
    let waits = !exits_within(&mut child, Duration::from_secs(20));
    let _ = child.kill();
    let lines = std::fs::read_to_string(&out).unwrap();
    let last = lines.lines().last().unwrap_or_default();
    assert!(
        !waits,
        "haest still waits 20 s later; out.jsonl ends with {last}\nthe terminal:\n{}",
        pty.screen()
    );
    assert!(victim.exists(), "../victim is gone");
    assert!(!lines.contains(r#""type":"ask""#), "asked into the file");
    let refused = |line: &&str| line.contains(r#""type":"notice""#) && line.contains("stdout");
    assert!(
        lines.lines().any(|l| refused(&l)),
        "the file does not say why: {lines}"
    );
    let shown = pty.screen();
    assert!(
        shown.contains("Denied") && shown.contains("stdout"),
        "{shown}"
    );
}

/// At the REPL prompt, while a background job's end waits to be taken, one
/// Ctrl+D quits nothing and runs nothing: haest says what quitting would
/// drop. The second quits, the waiting turn still not run (it would call
/// the model, and run `touch RAN` in the project).
#[test]
fn ctrl_d_at_the_prompt_never_runs_the_turn_that_waits() {
    let home = Home::new("ctrl-d-waits");
    let ran = home.0.join("w/RAN");
    let job = json!({"command": "sleep 2", "description": "wait", "run_in_background": true});
    let model = FakeModel::new([
        Reply::Tool("Bash".into(), job),
        Reply::Text("started a job".into()),
        bash("touch RAN"),
        Reply::Text("ran a turn on quit".into()),
    ]);
    home.serve(&model);
    let (mut pty, end) = Pty::open();
    let mut child = haest_on(&home, &["start a job"], &end).spawn().unwrap();
    let waits = |s: &str| s.contains("haest takes it when you press Enter.");
    let arrived = pty.wait_for(Duration::from_secs(30), waits);
    assert!(arrived, "the job's end never waited:\n{}", pty.screen());
    // Past the prompt's own start (it asks the terminal where its cursor is).
    std::thread::sleep(Duration::from_millis(300));
    pty.type_keys("\x04");
    let warned = |s: &str| s.contains("have not run yet");
    pty.wait_for(Duration::from_secs(10), |s| warned(s) || ran.exists());
    assert!(!ran.exists(), "one Ctrl+D ran the turn:\n{}", pty.screen());
    assert!(warned(&pty.screen()), "no warning:\n{}", pty.screen());
    // The prompt again, below the warning.
    let prompt = |s: &str| {
        s.find("have not run yet")
            .is_some_and(|at| s[at..].contains("> "))
    };
    assert!(
        pty.wait_for(Duration::from_secs(10), prompt),
        "{}",
        pty.screen()
    );
    std::thread::sleep(Duration::from_millis(300));
    pty.type_keys("\x04");
    let quit = exits_within(&mut child, Duration::from_secs(20));
    let _ = child.kill();
    assert!(quit, "a second Ctrl+D did not quit:\n{}", pty.screen());
    assert!(!ran.exists(), "quitting ran the turn:\n{}", pty.screen());
}
