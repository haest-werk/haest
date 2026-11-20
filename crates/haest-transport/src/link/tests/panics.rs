//! The backend is host code a connection runs, as its sink and its source
//! are: a bug in it reaches the host, never reads as the peer leaving.

use super::*;
use crate::testing::Hooked;

/// The connection stops, then `serve` raises the host's panic.
async fn stops_then_raises(mut peer: Peer) {
    let stopped = async {
        while peer.next().await.is_some() {}
        peer.served.await
    };
    let served = tokio::time::timeout(Duration::from_secs(5), stopped).await;
    let panic = served
        .expect("the connection stopped")
        .expect_err("serve returned instead of panicking");
    assert!(panic.is_panic(), "{panic:?}");
}

/// A panic in the host's backend while it serves a request: the connection
/// stops, and the panic reaches the host.
#[tokio::test]
async fn a_panic_in_the_hosts_backend_reaches_the_host() {
    let (s, _) = session(&answers(&[]), hub());
    let grant = Grant::watch(Origin::Agent);
    let mut peer = Peer::attach(Hooked::panicking(s.clone()), grant, Start::default());

    peer.send(json!({"v": 1, "id": "h", "type": "history"}))
        .await;

    stops_then_raises(peer).await;
    assert!(s.snapshot().await.is_ok(), "the conversation still runs");
    s.close().await;
}

/// A panic in the host's backend as it subscribes, on the connection's own
/// task: for an `events` request, and as a connection starts following the
/// stream (a parent on stdio). Either way the connection stops, and the
/// panic reaches the host.
#[tokio::test]
async fn a_panic_in_the_hosts_subscribe_reaches_the_host() {
    let (s, _) = session(&answers(&[]), hub());
    let grant = Grant::watch(Origin::Agent);
    let backend = Hooked::panicking_on_subscribe(s.clone());
    let mut peer = Peer::attach(backend, grant, Start::default());

    peer.send(json!({"v": 1, "id": "e", "type": "events", "after": 0}))
        .await;
    stops_then_raises(peer).await;

    let parent = Peer::parent(Hooked::panicking_on_subscribe(s.clone()));
    stops_then_raises(parent).await;

    assert!(s.snapshot().await.is_ok(), "the conversation still runs");
    s.close().await;
}
