//! The backend is host code a connection runs, as its sink and its source
//! are: a bug in it reaches the host, and never leaves a peer waiting.

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

/// A panic in the host's backend while it serves a request: that request is
/// still answered (refused, with its id), the connection stops, and the
/// panic reaches the host once it has.
#[tokio::test]
async fn a_panic_in_the_hosts_backend_answers_its_request_and_reaches_the_host() {
    let (s, _) = session(&answers(&[]), hub());
    let grant = Grant::watch(Origin::Agent);
    let mut peer = Peer::attach(Hooked::panicking(s.clone()), grant, Start::default());

    peer.send(json!({"v": 1, "id": "h", "type": "history"}))
        .await;
    let replied = tokio::time::timeout(Duration::from_secs(2), peer.reply("h")).await;
    let reply = replied.expect("the request was answered").pop().unwrap();
    assert_eq!(reply["ok"], false, "{reply}");

    stops_then_raises(peer).await;
    assert!(s.snapshot().await.is_ok(), "the conversation still runs");
    s.close().await;
}

/// A panic in the host's backend as it subscribes for an `events` request,
/// on the connection's own task: the request is refused with its id, the
/// connection stops, and the panic reaches the host once it has. One as a
/// connection starts following the stream (a parent on stdio) stops it the
/// same way, after its `hello`.
#[tokio::test]
async fn a_panic_in_the_hosts_subscribe_answers_its_events_request_and_reaches_the_host() {
    let (s, _) = session(&answers(&[]), hub());
    let grant = Grant::watch(Origin::Agent);
    let backend = Hooked::panicking_on_subscribe(s.clone());
    let mut peer = Peer::attach(backend, grant, Start::default());

    peer.send(json!({"v": 1, "id": "e", "type": "events", "after": 0}))
        .await;
    let answered = async {
        while let Some(frame) = peer.next().await {
            if is_reply(&frame, "e") {
                return Some(frame);
            }
        }
        None
    };
    let replied = tokio::time::timeout(Duration::from_secs(2), answered).await;
    let reply = replied.expect("the connection stopped");
    let reply = reply.expect("the events request got no reply");
    assert_eq!(reply["ok"], false, "{reply}");
    stops_then_raises(peer).await;

    let mut parent = Peer::parent(Hooked::panicking_on_subscribe(s.clone()));
    let hello = parent.next().await.expect("the hello went out");
    assert_eq!(ty(&hello), "hello", "{hello}");
    stops_then_raises(parent).await;

    assert!(s.snapshot().await.is_ok(), "the conversation still runs");
    s.close().await;
}
