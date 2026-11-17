//! A Unix socket transport outlives a failed accept: a process out of file
//! descriptors for a moment must not take the conversation's control socket
//! down for the rest of the conversation.
//!
//! A test binary of its own: it lowers the descriptor limit of the whole
//! process, which would fail any test running beside it.

#![cfg(all(unix, feature = "unix"))]

use std::{fs::File, os::fd::AsRawFd, time::Duration};

use haest_core::{
    Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools, testing::ScriptedProvider,
};
use haest_session::{Hub, start};
use haest_transport::{Grant, UnixSocket, connect_session};
use haest_wire::Origin;

/// The process's soft limit on open descriptors, set to `to` first when
/// given; the limit before.
#[expect(
    unsafe_code,
    reason = "getrlimit(2) and setrlimit(2): std cannot change the descriptor limit"
)]
fn soft_nofile(to: Option<libc::rlim_t>) -> libc::rlim_t {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes the struct it is handed, which lives here.
    let got = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut lim) };
    assert_eq!(got, 0);
    let was = lim.rlim_cur;
    if let Some(to) = to {
        lim.rlim_cur = to;
        // SAFETY: setrlimit reads the struct it is handed, which lives here.
        let set = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const lim) };
        assert_eq!(set, 0);
    }
    was
}

/// The lowest free descriptor: about how many are open.
fn next_fd() -> libc::rlim_t {
    let f = File::open("/dev/null").unwrap();
    libc::rlim_t::try_from(f.as_raw_fd()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_accept_leaves_the_socket_serving() {
    let dir = std::env::temp_dir().join(format!("hx-accept-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let model = ScriptedProvider::new([]);
    let shared = SharedDeps::new(model.resolver(), "/", ShutWorkspace);
    let agent = Agent::new(
        AgentDeps::new(shared, Tools::new(), ""),
        AgentOptions::new("scripted:m"),
    );
    let session = start(agent, Hub::new("s1"));
    let attached = UnixSocket::new(&dir)
        .attach(session.clone(), Grant::watch(Origin::Agent))
        .unwrap();

    // The process runs out of descriptors: every one is taken but the one a
    // client connects with, so the transport's accept fails (EMFILE).
    let was = soft_nofile(None);
    soft_nofile(Some(next_fd() + 32));
    let mut taken = Vec::new();
    while let Ok(f) = File::open("/dev/null") {
        taken.push(f);
    }
    taken.pop();
    let waiting = std::os::unix::net::UnixStream::connect(dir.join("s1.sock")).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(taken);
    soft_nofile(Some(was));

    // The pressure passed: the socket still serves.
    let peer = connect_session(&dir, "s1").await;
    assert!(peer.is_ok(), "{:?}", peer.err());
    drop((peer, waiting));
    let ended = tokio::time::timeout(Duration::from_millis(300), attached.ended()).await;
    assert!(ended.is_err(), "the transport ended: {ended:?}");
    session.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
