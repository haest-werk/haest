//! A panic in the host's backend reaches the host from the transport's
//! `ended`, whenever a connection meets it, and the transport's files go.

use std::{sync::Arc, time::Duration};

use haest_wire::{Origin, Request};
use tokio::sync::Notify;

use super::{Dir, UnixSocket, connect_session};
use crate::{
    Attached, Grant, TransportError,
    testing::{Hooked, answers, hub, session},
};

/// The transport stops, `ended` raises the host's panic, and its files go.
async fn ends_with_the_panic(attached: Attached, dir: &Dir) {
    let ended = tokio::spawn(attached.ended());
    let ended = tokio::time::timeout(Duration::from_secs(5), ended).await;
    let panic = ended
        .expect("the transport stopped")
        .expect_err("ended returned instead of panicking");
    assert!(panic.is_panic(), "{panic:?}");
    assert!(!dir.0.join("s1.sock").exists() && !dir.0.join("s1.json").exists());
}

/// A panic in the host's backend while it serves a connection: the
/// connection ends before the reply, and the panic reaches the host from
/// `ended`, never lost in the transport's task; the transport stops, and its
/// files go.
#[tokio::test]
async fn a_panic_in_the_hosts_backend_reaches_the_host() {
    let dir = Dir::new("panic");
    let (s, _) = session(&answers(&[]), hub());
    let attached = UnixSocket::new(&dir.0)
        .attach(Hooked::panicking(s.clone()), Grant::watch(Origin::Agent))
        .unwrap();
    let mut peer = connect_session(&dir.0, "s1").await.unwrap();

    let asked = peer.call(Request::History, |_| {});
    let asked = tokio::time::timeout(Duration::from_secs(2), asked).await;
    let asked = asked.expect("the connection ended");
    assert!(matches!(asked, Err(TransportError::Ended)), "{asked:?}");

    ends_with_the_panic(attached, &dir).await;
    s.close().await;
}

/// A panic in the host's backend a moment after the conversation ended,
/// while the transport gives its connections their last second: it still
/// reaches the host from `ended`, and the files go.
#[tokio::test]
async fn a_panic_in_the_hosts_backend_after_the_end_reaches_the_host() {
    let dir = Dir::new("late-panic");
    let (s, _) = session(&answers(&[]), hub());
    // A `history` that waits for the conversation's end, then panics.
    let asked = Arc::new(Notify::new());
    let heard = asked.clone();
    let backend = Hooked::new(s.clone(), move |inner| {
        let heard = heard.clone();
        Box::pin(async move {
            let mut end = inner.subscribe(u64::MAX);
            heard.notify_one();
            while end.recv().await.is_some() {}
            tokio::time::sleep(Duration::from_millis(100)).await;
            panic!("the host's backend has a bug");
        })
    });
    let attached = UnixSocket::new(&dir.0)
        .attach(backend, Grant::watch(Origin::Agent))
        .unwrap();
    let mut peer = connect_session(&dir.0, "s1").await.unwrap();
    peer.send(Request::History).await.unwrap();
    let started = tokio::time::timeout(Duration::from_secs(2), asked.notified()).await;
    started.expect("the history call started");

    s.close().await;
    ends_with_the_panic(attached, &dir).await;
}
