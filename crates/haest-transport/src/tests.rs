//! The errors a connection meets, as a host shows them.

use std::error::Error as _;

use haest_core::testing::assert_causes_said_once;
use haest_wire::{DecodeError, decode};

use crate::TransportError;

/// Each error with a cause says its own words; the chain says the cause,
/// once.
#[test]
fn every_error_with_a_cause_says_it_once() {
    let denied = || std::io::Error::other("permission denied");
    let not_json = || serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    assert_causes_said_once(&TransportError::Path {
        what: "bind",
        path: "/run/x.sock".into(),
        source: denied(),
    });
    assert_causes_said_once(&TransportError::NotAFrame(not_json()));
    assert_causes_said_once(&TransportError::Encode(not_json()));
    assert_causes_said_once(&TransportError::WebSocket(Box::new(denied())));
    assert_causes_said_once(&DecodeError::Json(not_json()));
    let shape = decode(r#"{"v": 1, "id": "x", "type": "nope"}"#).unwrap_err();
    assert!(
        shape.source().is_some(),
        "serde's reason is the cause: {shape:?}"
    );
    assert_causes_said_once(&shape);
}

/// A host generic over its transport attaches through the trait, which
/// does what the transport's own `attach` does.
#[cfg(feature = "stdio")]
#[tokio::test]
async fn the_trait_attaches_as_the_transports_own_attach_does() {
    use haest_wire::Origin;
    use tokio::io::AsyncReadExt;

    use crate::{Attached, Grant, Stdio, Transport, testing};

    fn through(t: impl Transport, b: haest_session::BackendRef) -> Attached {
        t.attach(b, Grant::watch(Origin::Agent))
            .expect("a pipe pair always attaches")
    }
    let (session, _go) = testing::session(&testing::answers(&[]), testing::hub());
    let (to_child, child_in) = tokio::io::duplex(1 << 16);
    let (child_out, mut from_child) = tokio::io::duplex(1 << 16);
    let attached = through(Stdio::new(child_in, child_out), session.into());
    drop(to_child);
    attached.ended().await;
    let mut out = String::new();
    from_child.read_to_string(&mut out).await.unwrap();
    assert!(out.contains(r#""type":"hello""#), "{out}");
}

/// Why a connection ended, in words a person reads; a failure with its
/// causes.
#[test]
fn an_ended_connection_says_why_in_words() {
    use crate::Ended;

    assert_eq!(Ended::PeerLeft.to_string(), "the peer left");
    assert_eq!(Ended::Closed.to_string(), "the conversation ended");
    let failed = Ended::Failed(TransportError::Path {
        what: "read",
        path: "/run/x.sock".into(),
        source: std::io::Error::other("reset by peer"),
    });
    assert_eq!(
        failed.to_string(),
        "reading from the peer failed: could not read /run/x.sock: reset by peer"
    );
}

/// A path error says what was tried on the path, not the path alone.
#[test]
fn a_path_error_says_what_failed_on_it() {
    let e = TransportError::Path {
        what: "connect to",
        path: "/run/x.sock".into(),
        source: std::io::Error::other("refused"),
    };
    assert_eq!(e.to_string(), "could not connect to /run/x.sock");
}
