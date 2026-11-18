//! `haest ctl` against a haest running with its control socket on, as a
//! second terminal meets it.

mod support;

use std::{
    io::{BufRead, BufReader, Write},
    os::{fd::OwnedFd, unix::fs::PermissionsExt, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    time::Duration,
};

use serde_json::{Value, json};
use support::{FakeModel, Home, LOCAL_KEY, Parent, Reply, bash, pty::Pty, stderr, ty};

/// A control dir short enough for a socket path (a temp dir's own path can
/// be most of the 104 bytes one may have); gone at the end.
struct ControlDir(PathBuf);

impl ControlDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(format!("/tmp/hc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }

    fn flag(&self) -> String {
        format!("--control-dir={}", self.0.display())
    }

    /// The one session's id, once its discovery file is there.
    fn sid(&self) -> String {
        for _ in 0..500 {
            let found = std::fs::read_dir(&self.0).ok().and_then(|entries| {
                entries
                    .filter_map(|e| e.ok()?.path().file_name()?.to_str().map(String::from))
                    .find_map(|n| n.strip_suffix(".json").map(String::from))
            });
            if let Some(sid) = found {
                return sid;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no discovery file in {}", self.0.display());
    }

    fn is_empty(&self) -> bool {
        std::fs::read_dir(&self.0).map_or(true, |mut d| d.next().is_none())
    }
}

impl Drop for ControlDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `haest ctl <args>` run to its end.
fn ctl(home: &Home, dir: &ControlDir, args: &[&str]) -> std::process::Output {
    let flag = dir.flag();
    home.haest(&[&["ctl"], args, &[flag.as_str()]].concat(), &[])
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn ctl_events_follow_shows_a_live_turn_until_the_run_ends() {
    let home = Home::new("ctl-follow");
    let dir = ControlDir::new("follow");
    let model = FakeModel::new([bash("sleep 2"), Reply::Text("all done".into())]);
    home.serve(&model);

    // A run in print mode, its turn kept going by a tool that sleeps.
    let flag = dir.flag();
    let args = [
        "-p",
        "go",
        "-m",
        "local:m",
        "--workspace",
        "real",
        "--tools",
        "bash",
        "--control-socket",
        "watch",
        flag.as_str(),
    ];
    let run = home
        .command(&args)
        .env(LOCAL_KEY.0, LOCAL_KEY.1)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let sid = dir.sid();
    assert_eq!(mode(&dir.0), 0o700);
    assert_eq!(mode(&dir.0.join(format!("{sid}.sock"))), 0o600);

    // A second terminal follows it, by a prefix of its id.
    let flag = dir.flag();
    let mut watch = home
        .command(&[
            "ctl",
            "events",
            &sid[..8],
            "--follow",
            "--json",
            flag.as_str(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let lines = BufReader::new(watch.stdout.take().unwrap());
    let frames: Vec<Value> = lines
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    // The watcher's stream ended with the run.
    assert!(watch.wait().unwrap().success());
    let run = run.wait_with_output().unwrap();
    assert_eq!(run.status.code(), Some(0), "{}", stderr(&run));
    assert_eq!(stdout(&run), "all done\n");

    let types: Vec<&str> = frames.iter().map(ty).collect();
    assert!(types.contains(&"tool_result"), "{types:?}");
    let said = frames.iter().rfind(|f| ty(f) == "assistant").unwrap();
    assert_eq!(said["message"]["content"][0]["text"], "all done");
    assert!(
        frames.iter().all(|f| f["sid"] == sid.as_str()),
        "one stream"
    );
    assert!(
        !types.contains(&"hello") && !types.contains(&"reply"),
        "the connection's own frames are not shown"
    );
    assert!(dir.is_empty(), "the socket and its discovery file are gone");
}

#[test]
fn a_command_socket_takes_input_and_an_interrupt_and_a_watch_one_refuses_them() {
    let home = Home::new("ctl-drive");
    let dir = ControlDir::new("drive");
    let model = FakeModel::new([Reply::Text("hello, other program".into()), bash("sleep 30")]);
    home.serve(&model);
    let flag = dir.flag();
    let mut host = Parent::start(
        &home,
        &[
            "--workspace",
            "real",
            "--tools",
            "bash",
            "--control-socket",
            "command",
            flag.as_str(),
        ],
    );
    let sid = dir.sid();

    // list and inspect see it.
    let out = ctl(&home, &dir, &["list", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let list: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(list[0]["sid"], sid.as_str());
    assert_eq!(list[0]["access"], "command");
    assert_eq!(list[0]["model"], "local:m");
    let out = ctl(&home, &dir, &["inspect", &sid, "--json"]);
    let now: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        (now["state"].as_str(), now["messages"].as_u64()),
        (Some("idle"), Some(0))
    );

    // send: a turn of its own, waited for.
    let out = ctl(&home, &dir, &["send", &sid, "hi", "there"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("The turn ended (end_turn)"),
        "{}",
        stderr(&out)
    );
    let sent = model.requests();
    let last = &sent[0]["messages"].as_array().unwrap().last().unwrap()["content"];
    assert!(last.to_string().contains("hi there"), "{last}");
    // The host's parent saw that turn in the stream.
    host.until(|f| {
        ty(f) == "assistant" && f["message"]["content"][0]["text"] == "hello, other program"
    });

    // interrupt: the parent's running turn stops.
    host.turn("t1", "sleep");
    host.until(|f| ty(f) == "assistant");
    std::thread::sleep(Duration::from_millis(300));
    let out = ctl(&home, &dir, &["interrupt", &sid[..6]]);
    assert!(out.status.success(), "{}", stderr(&out));
    let frames = host.reply("t1");
    assert_eq!(
        frames.last().unwrap()["result"]["stopReason"],
        "aborted",
        "{frames:#?}"
    );

    let (code, err) = host.finish();
    assert_eq!(code, Some(0), "{err}");
    assert!(dir.is_empty());
    let out = ctl(&home, &dir, &["list"]);
    assert!(
        stderr(&out).contains("No haest with a control socket"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_watch_socket_refuses_send_and_an_unknown_id_says_where_to_look() {
    let home = Home::new("ctl-watch");
    let dir = ControlDir::new("watch");
    let model = FakeModel::new([]);
    home.serve(&model);
    let flag = dir.flag();
    let mut host = Parent::start(&home, &["--control-socket", "watch", flag.as_str()]);
    let sid = dir.sid();
    // The host said it opened, and how to follow it.
    let open = host.until(|f| {
        ty(f) == "notice"
            && f["text"]
                .as_str()
                .unwrap()
                .starts_with("Control socket open")
    });
    assert!(
        open.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains(&sid[..8])
    );

    let out = ctl(&home, &dir, &["send", &sid, "hi"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("may only watch"), "{}", stderr(&out));
    assert!(model.requests().is_empty(), "no turn ran");
    let out = ctl(&home, &dir, &["interrupt", &sid]);
    assert_eq!(out.status.code(), Some(1));

    let out = ctl(&home, &dir, &["inspect", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("haest ctl list lists them"),
        "{}",
        stderr(&out)
    );

    host.send(json!({"v": 1, "id": "x", "type": "control", "verb": "close"}));
    let (code, err) = host.finish();
    assert_eq!(code, Some(0), "{err}");
}

/// A peer on the session's socket sends `close`: the connection, to read
/// its reply from.
fn send_close(dir: &ControlDir, sid: &str) -> UnixStream {
    let socket = dir.0.join(format!("{sid}.sock"));
    let mut peer = UnixStream::connect(socket).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let close = json!({"v": 1, "id": "c1", "type": "control", "verb": "close"});
    writeln!(peer, "{close}").unwrap();
    peer
}

/// A peer on the session's socket sends `close`, and waits for its reply:
/// true when the reply came, ok.
fn peer_closes(dir: &ControlDir, sid: &str) -> bool {
    BufReader::new(send_close(dir, sid))
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .find(|f| ty(f) == "reply" && f["id"] == "c1")
        .is_some_and(|reply| reply["ok"] == true)
}

/// `haest <args>` at a terminal (`pty_end` its stdin, stdout and stderr),
/// with a command socket in `dir`, in the default sandbox.
fn at_a_prompt(home: &Home, dir: &ControlDir, args: &[&str], pty_end: &OwnedFd) -> Child {
    let flag = dir.flag();
    let socket = ["--control-socket", "command", flag.as_str()];
    let on_the_terminal = || Stdio::from(pty_end.try_clone().unwrap());
    home.command(&[&["-m", "local:m", "--tools", "bash"], &socket[..], args].concat())
        .env(LOCAL_KEY.0, LOCAL_KEY.1)
        .stdin(on_the_terminal())
        .stdout(on_the_terminal())
        .stderr(on_the_terminal())
        .spawn()
        .unwrap()
}

/// Waits up to 20 s for `child` to exit: true when it has.
fn exits(child: &mut Child) -> bool {
    for _ in 0..200 {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// What the screen `s` showed from byte `at` on (at a character boundary).
fn since(s: &str, at: usize) -> &str {
    let at = (at..=s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len());
    &s[at..]
}

/// A peer closes the conversation while the human's prompt is open and the
/// sandbox holds a file the model wrote. The close is served as ever; the
/// terminal says at once, with no key pressed, that the conversation ended
/// and names the file, and the human can still /apply it before leaving.
#[test]
fn a_peers_close_at_the_prompt_leaves_the_sandbox_to_the_human() {
    let home = Home::new("ctl-close-sandbox");
    let dir = ControlDir::new("closesb");
    let model = FakeModel::new([
        bash("echo sandboxed > new.txt"),
        Reply::Text("wrote new.txt".into()),
    ]);
    home.serve(&model);
    let (mut pty, end) = Pty::open();
    let mut child = at_a_prompt(&home, &dir, &["write new.txt"], &end);
    let prompt = |s: &str| {
        s.find("wrote new.txt")
            .is_some_and(|at| s[at..].contains("> "))
    };
    let ready = pty.wait_for(Duration::from_secs(30), prompt);
    assert!(ready, "no prompt after the turn:\n{}", pty.screen());
    // Past the prompt's own start (it asks the terminal where its cursor is).
    std::thread::sleep(Duration::from_millis(300));
    let before = pty.screen().len();

    assert!(peer_closes(&dir, &dir.sid()), "the close was not served");
    let said = pty.wait_for(Duration::from_secs(10), |s| {
        since(s, before).contains("new.txt")
    });
    assert!(
        said,
        "the terminal did not name new.txt after the close:\n{}",
        since(&pty.screen(), before)
    );

    pty.type_keys("/apply\n");
    let file = home.0.join("w/new.txt");
    for _ in 0..100 {
        if file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let applied = std::fs::read_to_string(&file).ok();
    pty.type_keys("\x04");
    let left = exits(&mut child);
    let _ = child.kill();
    assert_eq!(
        applied.as_deref(),
        Some("sandboxed\n"),
        "/apply after the close did not write new.txt:\n{}",
        pty.screen()
    );
    assert!(left, "haest did not exit on Ctrl+D:\n{}", pty.screen());
}

/// A peer closes a conversation with nothing in the sandbox: it ends at
/// once, as it always did, and the open prompt says so at once, not at the
/// next Enter; Enter then leaves.
#[test]
fn a_peers_close_with_nothing_to_lose_ends_the_prompt_at_once() {
    let home = Home::new("ctl-close-nothing");
    let dir = ControlDir::new("closeno");
    let model = FakeModel::new([Reply::Text("hello".into())]);
    home.serve(&model);
    let (mut pty, end) = Pty::open();
    let mut child = at_a_prompt(&home, &dir, &["say hello"], &end);
    let prompt = |s: &str| s.find("hello").is_some_and(|at| s[at..].contains("> "));
    let ready = pty.wait_for(Duration::from_secs(30), prompt);
    assert!(ready, "no prompt after the turn:\n{}", pty.screen());
    std::thread::sleep(Duration::from_millis(300));
    let before = pty.screen().len();

    assert!(peer_closes(&dir, &dir.sid()), "the close was not served");
    let said = pty.wait_for(Duration::from_secs(10), |s| {
        since(s, before).to_lowercase().contains("ended")
    });
    assert!(
        said,
        "the prompt did not say the conversation ended:\n{}",
        since(&pty.screen(), before)
    );
    pty.type_keys("\n");
    let left = exits(&mut child);
    let _ = child.kill();
    assert!(left, "haest did not exit on Enter:\n{}", pty.screen());
    assert!(dir.is_empty(), "the socket is gone");
}

#[test]
fn a_project_file_cannot_open_a_control_socket() {
    let home = Home::new("ctl-project");
    // Even a project you trust.
    let user = home.0.join(".config/haest");
    std::fs::create_dir_all(&user).unwrap();
    let w = std::fs::canonicalize(home.0.join("w")).unwrap();
    std::fs::write(
        user.join("config.toml"),
        format!("trusted_dirs = [{:?}]\n", w.display().to_string()),
    )
    .unwrap();
    let project = home.0.join("w/.haest");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("config.toml"),
        "control_socket = \"command\"\n",
    )
    .unwrap();
    let out = home.haest(&["settings"], &[]);
    assert_ne!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(stderr(&out).contains("control_socket"), "{}", stderr(&out));
}
