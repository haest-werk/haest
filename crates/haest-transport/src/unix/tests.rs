//! The control socket, as another program on the machine meets it: found by
//! its discovery file, reached by its socket, private to its user, gone when
//! the conversation is.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use haest_core::testing::{text_answer, tool_answer};
use haest_session::{Backend, Body, Hub, HubOptions, Input};
use haest_wire::{Access, Level, Origin, Outbound, Request, Verb};
use serde_json::json;

use super::{SocketDiscovery, UnixPeer, UnixSocket, connect_session, running_sessions};
use crate::{
    Grant, Transport, TransportError,
    testing::{answers, hub, session},
};

/// A dir of its own, short enough for a socket path; gone at the end.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Dir(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

async fn eventually(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn the_dir_is_0700_the_socket_0600_and_the_discovery_file_says_where() {
    let dir = Dir::new("perms");
    let (s, _) = session(&answers(&[]), hub());
    let attached = UnixSocket::new(&dir.0)
        .attach(Arc::new(s), Grant::watch(Origin::Agent))
        .unwrap();

    let socket = dir.0.join("s1.sock");
    let file = dir.0.join("s1.json");
    assert_eq!(mode(&dir.0), 0o700);
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(&file), 0o600);
    let found: SocketDiscovery = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(found.sid, "s1");
    assert_eq!(found.pid, std::process::id());
    assert_eq!(found.access, Access::Watch);
    assert_eq!(found.socket, socket);

    // Detached: both files go.
    attached.detach();
    eventually("the files are removed", || {
        !socket.exists() && !file.exists()
    })
    .await;
}

#[tokio::test]
async fn a_dir_that_was_open_is_made_private() {
    let dir = Dir::new("open");
    std::fs::create_dir_all(&dir.0).unwrap();
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (s, _) = session(&answers(&[]), hub());
    let _attached = UnixSocket::new(&dir.0)
        .attach(Arc::new(s), Grant::watch(Origin::Agent))
        .unwrap();
    assert_eq!(mode(&dir.0), 0o700);
}

#[tokio::test]
async fn a_stale_socket_from_a_crashed_run_is_replaced() {
    let dir = Dir::new("stale");
    std::fs::create_dir_all(&dir.0).unwrap();
    // A crashed run's socket: bound once, nobody listening now.
    drop(std::os::unix::net::UnixListener::bind(dir.0.join("s1.sock")).unwrap());
    assert!(dir.0.join("s1.sock").exists());

    let (s, _) = session(&answers(&[]), hub());
    let _attached = UnixSocket::new(&dir.0)
        .attach(Arc::new(s), Grant::watch(Origin::Agent))
        .unwrap();
    let mut peer = connect_session(&dir.0, "s1").await.unwrap();
    let now = peer.call(Request::Snapshot, |_| {}).await.unwrap().unwrap();
    assert_eq!(now["model"], "scripted:m");
}

#[tokio::test]
async fn a_socket_a_running_session_serves_is_not_taken() {
    let dir = Dir::new("taken");
    let (s, _) = session(&answers(&[]), hub());
    let backend: Arc<dyn Backend> = Arc::new(s);
    let _first = UnixSocket::new(&dir.0)
        .attach(backend.clone(), Grant::watch(Origin::Agent))
        .unwrap();
    let second = UnixSocket::new(&dir.0).attach(backend, Grant::watch(Origin::Agent));
    assert!(
        matches!(second, Err(TransportError::InUse(_))),
        "{second:?}"
    );
    assert!(
        dir.0.join("s1.sock").exists(),
        "the first one's socket stays"
    );
}

#[tokio::test]
async fn running_lists_the_live_sessions_and_clears_what_a_crashed_run_left() {
    let dir = Dir::new("list");
    let (s, _) = session(&answers(&[]), Hub::new("live-1", HubOptions::default()));
    let _attached = UnixSocket::new(&dir.0)
        .attach(Arc::new(s), Grant::command(Origin::Agent))
        .unwrap();
    // A crashed run: its discovery file, a socket nobody answers on.
    let dead = dir.0.join("dead-1.sock");
    drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
    let zombie = SocketDiscovery {
        sid: "dead-1".into(),
        pid: u32::MAX,
        started_ms: 1,
        access: Access::Watch,
        socket: dead.clone(),
    };
    std::fs::write(
        dir.0.join("dead-1.json"),
        serde_json::to_vec(&zombie).unwrap(),
    )
    .unwrap();
    // Not a discovery file at all.
    std::fs::write(dir.0.join("notes.json"), "not json").unwrap();

    let found = running_sessions(&dir.0).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].discovery.sid, "live-1");
    assert_eq!(found[0].discovery.access, Access::Command);
    assert_eq!(found[0].model_spec(), "scripted:m");
    assert!(
        !dir.0.join("dead-1.json").exists() && !dead.exists(),
        "the crashed run's files are gone"
    );
    assert!(
        running_sessions(&dir.0.join("nothing-here"))
            .await
            .is_empty()
    );
}

/// A discovery file for `sid` in `dir` that names `socket`.
fn discovery_file(dir: &Path, sid: &str, socket: &Path) -> PathBuf {
    let file = dir.join(format!("{sid}.json"));
    let d = SocketDiscovery {
        sid: sid.into(),
        pid: u32::MAX,
        started_ms: 1,
        access: Access::Watch,
        socket: socket.into(),
    };
    std::fs::write(&file, serde_json::to_vec(&d).unwrap()).unwrap();
    file
}

/// Only a socket nothing answers on, at the path the transport gives it
/// beside its own discovery file, is cleared: never a path a discovery file
/// merely names, never one that failed for another reason.
#[tokio::test]
async fn running_clears_only_a_dead_socket_beside_its_own_discovery_file() {
    let dir = Dir::new("clear");
    std::fs::create_dir_all(dir.0.join("elsewhere")).unwrap();
    // A crafted discovery file naming a file that is not its socket.
    let victim = dir.0.join("elsewhere/victim.txt");
    std::fs::write(&victim, "keep me").unwrap();
    let crafted = discovery_file(&dir.0, "crafted", &victim);
    // A socket at the right path that fails for another reason than "nobody
    // answers" (here: a path too long for a socket address).
    let long = "l".repeat(120);
    let busy = dir.0.join(format!("{long}.sock"));
    std::fs::write(&busy, "").unwrap();
    let kind = std::os::unix::net::UnixStream::connect(&busy)
        .unwrap_err()
        .kind();
    assert_eq!(kind, std::io::ErrorKind::InvalidInput);
    let busy_file = discovery_file(&dir.0, &long, &busy);
    // A crashed run whose socket is already gone: its discovery file goes.
    let gone = discovery_file(&dir.0, "gone", &dir.0.join("gone.sock"));

    assert!(running_sessions(&dir.0).await.is_empty());
    assert!(victim.exists(), "a file a discovery file names is kept");
    assert!(crafted.exists(), "and so is the discovery file");
    assert!(
        busy.exists() && busy_file.exists(),
        "a transient failure keeps both"
    );
    assert!(!gone.exists(), "a run whose socket is gone is cleared");
}

#[tokio::test]
async fn a_session_is_reached_by_its_id_or_a_prefix_only_it_has() {
    let dir = Dir::new("prefix");
    let mut kept = Vec::new();
    for sid in ["abc-1", "abd-2"] {
        let (s, _) = session(&answers(&[]), Hub::new(sid, HubOptions::default()));
        kept.push(
            UnixSocket::new(&dir.0)
                .attach(Arc::new(s), Grant::watch(Origin::Agent))
                .unwrap(),
        );
    }
    let sid = |peer: &UnixPeer| peer.hello().sid.clone();
    assert_eq!(
        sid(&connect_session(&dir.0, "abc-1").await.unwrap()),
        "abc-1"
    );
    assert_eq!(sid(&connect_session(&dir.0, "abd").await.unwrap()), "abd-2");
    assert!(matches!(
        connect_session(&dir.0, "ab").await,
        Err(TransportError::Ambiguous(_))
    ));
    assert!(matches!(
        connect_session(&dir.0, "zzz").await,
        Err(TransportError::NoSession(_))
    ));
}

#[tokio::test]
async fn a_watch_socket_shows_the_stream_and_refuses_to_drive() {
    let dir = Dir::new("watch");
    let model = answers(&["first", "second"]);
    let (s, _) = session(&model, hub());
    let backend: Arc<dyn Backend> = Arc::new(s);
    let _attached = UnixSocket::new(&dir.0)
        .attach(backend.clone(), Grant::watch(Origin::Agent))
        .unwrap();
    backend.turn(Input::human("one")).await.unwrap();

    let mut peer = connect_session(&dir.0, "s1").await.unwrap();
    assert_eq!(peer.access(), Access::Watch);
    let turn = peer.call(Request::Turn { text: "hi".into() }, |_| {}).await;
    assert!(
        matches!(&turn, Err(TransportError::Refused(why)) if why.contains("may only watch")),
        "{turn:?}"
    );
    let stop = Request::Control {
        verb: Verb::Interrupt,
        model_spec: None,
        effort: None,
    };
    assert!(matches!(
        peer.call(stop, |_| {}).await,
        Err(TransportError::Refused(_))
    ));

    // The history so far, then the reply; then, following, the next turn live.
    let mut seen = Vec::new();
    let follow = Request::Events {
        after: 0,
        follow: true,
    };
    peer.call(follow, |f| seen.push(f.clone())).await.unwrap();
    let last = seen.last().unwrap().seq;
    assert!(seen.iter().any(|f| matches!(&f.body, Body::Loop(haest_core::LoopEvent::Assistant { message }) if message.text() == "first")));

    backend.submit(Input::human("two")).await.unwrap();
    let mut live = Vec::new();
    while let Some(frame) = peer.recv().await {
        let frame = frame.unwrap();
        let end = matches!(
            frame.body,
            Body::Loop(haest_core::LoopEvent::TurnEnd { .. })
        );
        live.push(frame);
        if end {
            break;
        }
    }
    assert_eq!(
        live[0].seq,
        last + 1,
        "contiguous: nothing lost, nothing twice"
    );
    assert!(live.iter().any(|f| matches!(&f.body, Body::Loop(haest_core::LoopEvent::Assistant { message }) if message.text() == "second")));
}

#[tokio::test]
async fn a_command_socket_drives_the_conversation() {
    let dir = Dir::new("command");
    let model = haest_core::testing::ScriptedProvider::new([
        text_answer("noted"),
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("never"),
    ]);
    let (s, _) = session(&model, hub());
    let backend: Arc<dyn Backend> = Arc::new(s);
    let _attached = UnixSocket::new(&dir.0)
        .attach(backend.clone(), Grant::command(Origin::Agent))
        .unwrap();

    // Input from the socket is a turn, another program's.
    let mut peer = connect_session(&dir.0, "s1").await.unwrap();
    let end = peer
        .call(
            Request::Turn {
                text: "X Y Z".into(),
            },
            |_| {},
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end["stopReason"], "end_turn");
    let history = backend.history().await.unwrap().messages;
    assert!(matches!(&history[0], haest_core::Message::User(u) if u.content == "X Y Z"));

    // An interrupt from a second connection stops a running turn.
    let turn = tokio::spawn({
        let backend = backend.clone();
        async move { backend.turn(Input::human("wait")).await }
    });
    eventually("the tool runs", || model.requests().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut other = connect_session(&dir.0, "s1").await.unwrap();
    let stop = Request::Control {
        verb: Verb::Interrupt,
        model_spec: None,
        effort: None,
    };
    other.call(stop, |_| {}).await.unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(end.stop_reason, haest_core::StopReason::Aborted);
}

#[tokio::test]
async fn a_late_watcher_whose_frames_the_ring_lost_gets_a_gap() {
    let dir = Dir::new("gap");
    let hub = Hub::new(
        "s1",
        HubOptions {
            ring: 2,
            buffer: 64,
        },
    );
    for n in 0..6 {
        hub.notice(Level::Info, format!("line {n}"));
    }
    let (s, _) = session(&answers(&[]), hub);
    let _attached = UnixSocket::new(&dir.0)
        .attach(Arc::new(s), Grant::watch(Origin::Agent))
        .unwrap();

    let mut peer = connect_session(&dir.0, "s1").await.unwrap();
    let mut seen = Vec::new();
    let replay = Request::Events {
        after: 0,
        follow: false,
    };
    peer.call(replay, |f| seen.push(f.clone())).await.unwrap();
    assert!(
        matches!(seen[0].body, Body::Wire(Outbound::Gap { after: 0, .. })),
        "{seen:#?}"
    );
    assert!(seen.len() <= 4, "the gap and what the ring kept: {seen:#?}");
}

#[tokio::test]
async fn closing_the_conversation_ends_the_transport_and_removes_its_files() {
    let dir = Dir::new("close");
    let (s, _) = session(&answers(&[]), hub());
    let backend: Arc<dyn Backend> = Arc::new(s);
    let attached = UnixSocket::new(&dir.0)
        .attach(backend.clone(), Grant::watch(Origin::Agent))
        .unwrap();
    let mut watcher = connect_session(&dir.0, "s1").await.unwrap();
    let follow = Request::Events {
        after: 0,
        follow: true,
    };
    watcher.call(follow, |_| {}).await.unwrap();

    backend.close().await;
    let ended = tokio::time::timeout(Duration::from_secs(5), attached.ended())
        .await
        .unwrap();
    assert!(matches!(ended, crate::Ended::Closed), "{ended:?}");
    assert!(!dir.0.join("s1.sock").exists() && !dir.0.join("s1.json").exists());
    // The watcher's stream ended with the conversation.
    let rest = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = watcher.recv().await {
            frame.unwrap();
        }
    });
    rest.await.unwrap();
}
