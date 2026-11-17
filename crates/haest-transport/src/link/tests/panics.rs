//! The backend is host code a connection runs, as its sink and its source
//! are: a bug in it reaches the host, and never leaves a peer waiting.

use super::*;
use crate::testing::Hooked;

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

    let stopped = async {
        while peer.next().await.is_some() {}
        peer.served.await
    };
    let served = tokio::time::timeout(Duration::from_secs(5), stopped).await;
    let panic = served
        .expect("the connection stopped")
        .expect_err("serve returned instead of panicking");
    assert!(panic.is_panic(), "{panic:?}");
    assert!(s.snapshot().await.is_ok(), "the conversation still runs");
    s.close().await;
}
