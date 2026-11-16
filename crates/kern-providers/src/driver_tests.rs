//! What every adapter shares: retries, refusals, a dead connection and
//! cancellation all end with exactly one `Done`, never a panic or a hang.

use std::{sync::Arc, time::Duration};

use kern_core::{
    messages::StopReason,
    provider::{Provider, ProviderEvent},
};
use serde_json::json;

use crate::{
    RetryPolicy,
    compat::{CompatFlavor, CompatOptions, CompatProvider},
    driver::parse_args,
    testing::{FakeEnd, FakeReply, FakeTransport, chat_sse, collect, kinds, request},
    transport::TransportError,
};

#[test]
fn tool_arguments_parse_blanks_are_empty_and_garbage_is_surfaced() {
    use serde_json::Value;
    assert_eq!(Value::Object(parse_args(r#"{"a":1}"#)), json!({"a": 1}));
    assert!(parse_args("  ").is_empty());
    assert_eq!(Value::Object(parse_args("5")), json!({"value": 5}));
    assert_eq!(
        Value::Object(parse_args("{bad")),
        json!({"_unparsed": "{bad"})
    );
}

fn quick() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
    }
}

fn provider(t: &Arc<FakeTransport>) -> CompatProvider {
    CompatProvider::new(
        CompatOptions::new("local", CompatFlavor::Generic, "k"),
        t.clone(),
    )
    .with_retry(quick())
}

fn hello() -> FakeReply {
    FakeReply::sse(chat_sse(&[
        json!({"choices": [{"index": 0, "delta": {"content": "hi"}}]}),
    ]))
}

#[tokio::test]
async fn overload_rate_limit_and_a_dead_connection_are_retried_then_the_answer_streams() {
    let t = FakeTransport::new([
        FakeReply::status(529, r#"{"error":{"message":"overloaded"}}"#),
        FakeReply::status(429, "slow down").header("retry-after-ms", "1"),
        FakeReply::Error(TransportError::connect("connection refused")),
        hello(),
    ]);
    let (events, msg) = collect(provider(&t).stream(request("m"))).await;
    assert_eq!(kinds(&events), ["text_delta", "done"]);
    assert_eq!(msg.stop_reason, StopReason::EndTurn);
    assert_eq!(msg.text(), "hi");
    assert_eq!(t.requests().len(), 4);
}

#[tokio::test]
async fn a_bad_request_is_not_retried_and_ends_as_an_error_naming_status_and_reason() {
    let t = FakeTransport::new([
        FakeReply::status(400, r#"{"error":{"message":"bad model id"}}"#),
        hello(),
    ]);
    let (events, msg) = collect(provider(&t).stream(request("m"))).await;
    assert_eq!(kinds(&events), ["done"]);
    assert_eq!(msg.stop_reason, StopReason::Error);
    let err = msg.error_message.unwrap();
    assert!(err.contains("400") && err.contains("bad model id"), "{err}");
    assert_eq!(t.requests().len(), 1);
}

#[tokio::test]
async fn the_server_can_veto_a_retry() {
    let t = FakeTransport::new([
        FakeReply::status(500, "no").header("x-should-retry", "false"),
        hello(),
    ]);
    let (_, msg) = collect(provider(&t).stream(request("m"))).await;
    assert_eq!(msg.stop_reason, StopReason::Error);
    assert_eq!(t.requests().len(), 1);
}

#[tokio::test]
async fn retries_run_out() {
    let t = FakeTransport::new((0..4).map(|_| FakeReply::status(503, "")));
    let (_, msg) = collect(provider(&t).stream(request("m"))).await;
    assert_eq!(msg.stop_reason, StopReason::Error);
    assert!(msg.error_message.unwrap().contains("503"));
    assert_eq!(t.requests().len(), 4);
}

#[tokio::test]
async fn a_body_that_breaks_off_is_not_retried_and_keeps_what_streamed() {
    let broken =
        FakeReply::sse("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"par\"}}]}\n\n")
            .then(FakeEnd::Fail("connection reset".into()));
    let t = FakeTransport::new([broken, hello()]);
    let (_, msg) = collect(provider(&t).stream(request("m"))).await;
    assert_eq!(msg.stop_reason, StopReason::Error);
    assert_eq!(msg.error_message.as_deref(), Some("connection reset"));
    assert_eq!(msg.text(), "par");
    assert_eq!(t.requests().len(), 1);
}

#[tokio::test]
async fn cancelling_during_a_retry_backoff_ends_as_aborted() {
    let t = FakeTransport::new([FakeReply::status(503, "")]);
    let slow = RetryPolicy {
        max_retries: 3,
        initial_delay: Duration::from_secs(3600),
        max_delay: Duration::from_secs(3600),
    };
    let p = CompatProvider::new(
        CompatOptions::new("local", CompatFlavor::Generic, "k"),
        t.clone(),
    )
    .with_retry(slow);
    let req = request("m");
    let cancel = req.cancel.clone();
    let stream = p.stream(req);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });
    let (_, msg) = tokio::time::timeout(Duration::from_secs(5), collect(stream))
        .await
        .expect("cancel ends the stream promptly");
    assert_eq!(msg.stop_reason, StopReason::Aborted);
    assert_eq!(msg.error_message, None);
}

#[tokio::test]
async fn a_cancelled_stream_ends_with_done_even_while_the_body_hangs() {
    // No `[DONE]`: only cancellation ends this body.
    let open =
        FakeReply::sse("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n");
    let t = FakeTransport::new([open.then(FakeEnd::Hang)]);
    let req = request("m");
    let cancel = req.cancel.clone();
    let mut stream = provider(&t).stream(req);
    use futures::StreamExt;
    let first = stream.next().await;
    assert_eq!(first, Some(ProviderEvent::TextDelta("hi".into())));
    cancel.cancel();
    let Some(ProviderEvent::Done(msg)) = stream.next().await else {
        panic!("no Done")
    };
    assert_eq!(msg.stop_reason, StopReason::Aborted);
    assert_eq!(msg.text(), "hi");
    assert_eq!(stream.next().await, None);
}
