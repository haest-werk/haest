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

/// A detached transport stops serving: a peer that follows the stream gets
/// nothing said after the detach, and what it reads ends.
#[cfg(feature = "stdio")]
#[tokio::test]
async fn a_detached_transport_writes_nothing_more() {
    use std::time::Duration;

    use haest_wire::{Level, Origin};

    use crate::{Grant, LineSource, NdjsonSource, Stdio, testing};

    let (session, _go) = testing::session(&testing::answers(&[]), testing::hub());
    let (_to_child, child_in) = tokio::io::duplex(1 << 16);
    let (child_out, from_child) = tokio::io::duplex(1 << 16);
    let attached =
        Stdio::new(child_in, child_out).attach(session.clone(), Grant::watch(Origin::Agent));
    let mut from_child = NdjsonSource::new(from_child);
    let hello = from_child.next_line().await.unwrap().unwrap();
    assert!(hello.contains(r#""type":"hello""#), "{hello}");

    attached.detach();
    tokio::time::sleep(Duration::from_millis(50)).await;
    session.hub().notice(Level::Info, "said after the detach");
    let mut rest = Vec::new();
    let read = async {
        while let Some(line) = from_child.next_line().await {
            rest.push(line.unwrap());
        }
    };
    let ended = tokio::time::timeout(Duration::from_secs(2), read).await;
    assert!(ended.is_ok(), "still served after the detach: {rest:?}");
    assert!(
        rest.iter().all(|l| !l.contains("said after the detach")),
        "{rest:?}"
    );
    session.close().await;
}

/// An output that holds the write of the line holding `hold`, blocking its
/// worker, until the test lets it go, so the connection is in the middle of
/// a step; it keeps every byte it was given, and says when it is dropped.
#[cfg(feature = "stdio")]
struct HeldOut {
    hold: &'static str,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    go: std::sync::mpsc::Receiver<()>,
    written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    gone: Option<tokio::sync::oneshot::Sender<()>>,
}

#[cfg(feature = "stdio")]
impl tokio::io::AsyncWrite for HeldOut {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let holds = String::from_utf8_lossy(buf).contains(this.hold);
        if holds && let Some(entered) = this.entered.take() {
            let _ = entered.send(());
            let _ = this.go.recv_timeout(std::time::Duration::from_secs(5));
        }
        this.written
            .lock()
            .expect("lock poisoned")
            .extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[cfg(feature = "stdio")]
impl Drop for HeldOut {
    fn drop(&mut self) {
        if let Some(gone) = self.gone.take() {
            let _ = gone.send(());
        }
    }
}

/// A detach while a connection is writing a line, on another worker, lets
/// nothing the conversation makes after it out. An abort lands only once
/// that step ends, and the writer went on to the next frame first.
#[cfg(feature = "stdio")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_detach_mid_write_lets_nothing_made_after_it_out() {
    use std::time::Duration;

    use haest_wire::{Level, Origin};
    use tokio::sync::oneshot;

    use crate::{Grant, Stdio, testing};

    let (session, _go) = testing::session(&testing::answers(&[]), testing::hub());
    // In the ring, so the connection's catch-up writes it.
    session.hub().notice(Level::Info, "hold here");
    let (_to_child, child_in) = tokio::io::duplex(1 << 16);
    let (entered_tx, entered) = oneshot::channel();
    let (go, held) = std::sync::mpsc::channel();
    let (gone_tx, gone) = oneshot::channel();
    let written = std::sync::Arc::default();
    let out = HeldOut {
        hold: "hold here",
        entered: Some(entered_tx),
        go: held,
        written: std::sync::Arc::clone(&written),
        gone: Some(gone_tx),
    };
    let attached = Stdio::new(child_in, out).attach(session.clone(), Grant::watch(Origin::Agent));
    let entered = tokio::time::timeout(Duration::from_secs(5), entered).await;
    assert!(
        matches!(entered, Ok(Ok(()))),
        "the held line was never written"
    );

    attached.detach();
    session.hub().notice(Level::Info, "said after the detach");
    // The stream reaches the connection's out queue meanwhile.
    tokio::time::sleep(Duration::from_millis(100)).await;
    go.send(()).expect("the output is still held");
    let gone = tokio::time::timeout(Duration::from_secs(2), gone).await;
    assert!(gone.is_ok(), "the output outlived the detach");

    let written = written.lock().expect("lock poisoned").clone();
    let written = String::from_utf8_lossy(&written);
    assert!(written.contains("hold here"), "{written}");
    assert!(!written.contains("said after the detach"), "{written}");
    session.close().await;
}

/// Why a connection ended, for a person: a failure with all its causes; a
/// broker's refusal as it is, since nothing was read.
#[test]
fn an_ended_connection_says_a_failure_with_its_causes() {
    use haest_core::error::report;

    use crate::Ended;

    let read = || TransportError::Path {
        what: "read",
        path: "/run/x.sock".into(),
        source: std::io::Error::other("reset by peer"),
    };
    let failed = Ended::Failed(read()).to_string();
    assert!(failed.ends_with(&report(&read())), "{failed}");
    assert_ne!(
        failed,
        report(&read()),
        "what failed comes before the causes"
    );
    // A broker that refused the token at dial time: nothing was read.
    let refused = || TransportError::Auth {
        code: 401,
        reason: "unknown token".into(),
    };
    assert_eq!(Ended::Failed(refused()).to_string(), report(&refused()));
}

/// A path error says what was tried on the path, not the path alone.
#[test]
fn a_path_error_says_what_failed_on_it() {
    let e = TransportError::Path {
        what: "connect to",
        path: "/run/x.sock".into(),
        source: std::io::Error::other("refused"),
    };
    assert!(e.to_string().contains("connect to /run/x.sock"), "{e}");
}
