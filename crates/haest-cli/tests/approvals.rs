//! A run at a terminal with a `command` control socket puts each approval
//! ask to the human at the terminal AND to the socket's peers: the first
//! answer wins, and the other side stops asking.

mod support;

use std::{
    io::{BufRead, BufReader, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::PathBuf,
    process::{Child, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use support::{ControlDir, FakeModel, Home, LOCAL_KEY, Reply, bash, pty::Pty, ty};

/// How the terminal's question ends, right where the answer goes.
const ASKED: &str = "[y]es / [a]lways / [N]o: ";

/// A project whose neighbour `victim` the model wants gone: it asks to run
/// `rm -rf ../victim`, then waits 3 s (`sleep 3`, which needs no approval:
/// the run is still there for a late answer), then says it is done.
fn victim_run(name: &str) -> (Home, FakeModel, PathBuf) {
    let home = Home::new(name);
    let victim = home.0.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "x").unwrap();
    let model = FakeModel::new([
        bash("rm -rf ../victim"),
        bash("sleep 3"),
        Reply::Text("done".into()),
    ]);
    home.serve(&model);
    (home, model, victim)
}

/// `haest -p` in the real workspace on the terminal `pty_end`, its control
/// socket `command` in `dir`.
fn at_the_terminal(home: &Home, dir: &ControlDir, pty_end: &OwnedFd) -> Child {
    let flag = dir.flag();
    let args = [
        "-p",
        "clean up",
        "-m",
        "local:m",
        "--workspace",
        "real",
        "--tools",
        "bash",
        "--control-socket",
        "command",
        flag.as_str(),
    ];
    let on_the_terminal = || Stdio::from(pty_end.try_clone().unwrap());
    home.command(&args)
        .env(LOCAL_KEY.0, LOCAL_KEY.1)
        .stdin(on_the_terminal())
        .stdout(on_the_terminal())
        .stderr(on_the_terminal())
        .spawn()
        .unwrap()
}

/// A program on the control socket that follows the stream.
struct SocketPeer {
    conn: UnixStream,
    frames: mpsc::Receiver<Value>,
}

impl SocketPeer {
    fn follow(dir: &ControlDir) -> Self {
        let socket = dir.0.join(format!("{}.sock", dir.sid()));
        let mut conn = UnixStream::connect(socket).unwrap();
        let events = json!({"v": 1, "id": "e1", "type": "events", "after": 0, "follow": true});
        writeln!(conn, "{events}").unwrap();
        let read = BufReader::new(conn.try_clone().unwrap());
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in read.lines().map_while(Result::ok) {
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });
        Self { conn, frames }
    }

    /// Frames up to and including the first `stop` matches.
    fn until(&self, stop: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            match self.frames.recv_timeout(Duration::from_secs(30)) {
                Ok(frame) => {
                    let done = stop(&frame);
                    seen.push(frame);
                    if done {
                        return seen;
                    }
                }
                Err(e) => panic!("{e} waiting; seen: {seen:#?}"),
            }
        }
    }

    fn answer(&mut self, id: &str, ask: &Value, answer: &str) {
        let line =
            json!({"v": 1, "id": id, "type": "approval_answer", "ask": ask, "answer": answer});
        writeln!(self.conn, "{line}").unwrap();
    }
}

/// Waits up to 30 s for `child` to exit: true when it has.
fn exits(child: &mut Child) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// The peer answers while the question waits at the terminal: the terminal
/// stops waiting and says another program answered, the call is denied, and
/// the stream names the call the ask held up and who answered it.
#[test]
fn a_peer_answers_the_terminals_question() {
    let (home, _model, victim) = victim_run("ask-peer");
    let dir = ControlDir::new("askpeer");
    let (pty, end) = Pty::open();
    let mut child = at_the_terminal(&home, &dir, &end);
    let mut peer = SocketPeer::follow(&dir);

    let asked = peer.until(|f| ty(f) == "approval_ask").pop().unwrap();
    assert_eq!(asked["toolCallId"], "call-1", "{asked}");
    let drawn = pty.wait_for(Duration::from_secs(30), |s| s.contains(ASKED));
    assert!(drawn, "the terminal never asked:\n{}", pty.screen());
    peer.answer("a1", &asked["ask"], "deny");

    let frames = peer.until(|f| ty(f) == "approval_answered");
    let answered = frames.last().unwrap();
    assert_eq!(
        (&answered["ask"], &answered["answer"], &answered["by"]),
        (&asked["ask"], &json!("deny"), &json!("peer")),
        "{answered}"
    );
    let twin = frames.iter().find(|f| ty(f) == "ask").unwrap();
    assert_eq!(twin["approval"], asked["ask"], "{twin}");
    let left = exits(&mut child);
    let _ = child.kill();
    let screen = pty.screen();
    assert!(left, "the terminal still waits for its answer:\n{screen}");
    assert!(victim.exists(), "../victim is gone:\n{screen}");
    assert!(screen.contains("Another program denied it."), "{screen}");
}

/// The human answers at the terminal: the peer is told at once who answered,
/// and its own answer after that is refused.
#[test]
fn the_terminal_answers_and_the_peer_is_told() {
    let (home, _model, victim) = victim_run("ask-term");
    let dir = ControlDir::new("askterm");
    let (mut pty, end) = Pty::open();
    let mut child = at_the_terminal(&home, &dir, &end);
    let mut peer = SocketPeer::follow(&dir);

    let asked = peer.until(|f| ty(f) == "approval_ask").pop().unwrap();
    let drawn = pty.wait_for(Duration::from_secs(30), |s| s.contains(ASKED));
    assert!(drawn, "the terminal never asked:\n{}", pty.screen());
    // Again until it lands: a key typed the instant the question is drawn
    // may be dropped with what was typed before it.
    let (tx, answered) = mpsc::channel();
    let typing = std::thread::spawn(move || {
        while answered.try_recv().is_err() {
            pty.type_keys("n\n");
            std::thread::sleep(Duration::from_millis(300));
        }
        pty
    });
    let frame = peer.until(|f| ty(f) == "approval_answered").pop().unwrap();
    let _ = tx.send(());
    let pty = typing.join().unwrap();
    assert_eq!(
        (&frame["ask"], &frame["answer"], &frame["by"]),
        (&asked["ask"], &json!("deny"), &json!("terminal")),
        "{frame}"
    );
    peer.answer("late", &asked["ask"], "allow");
    let reply = peer
        .until(|f| ty(f) == "reply" && f["id"] == "late")
        .pop()
        .unwrap();
    assert_eq!(reply["ok"], false, "a late answer was taken: {reply}");
    let left = exits(&mut child);
    let _ = child.kill();
    assert!(left, "haest still runs:\n{}", pty.screen());
    assert!(victim.exists(), "../victim is gone:\n{}", pty.screen());
}
