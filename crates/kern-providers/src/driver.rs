//! The one stream loop every dialect shares: send the request (with retries),
//! cut the body into SSE events, hand each to the dialect's fold, and end with
//! exactly one `Done`. A failure anywhere -- refused request, broken body, an
//! error event, cancellation -- still ends with `Done`; the fold decides what
//! of the half-built message survives.

use std::sync::Arc;

use futures::StreamExt;
use kern_core::{
    messages::AssistantMessage,
    provider::{ProviderEvent, ProviderStream},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    http::{self, RetryPolicy},
    sse::{SseEvent, SseParser},
    transport::{HttpRequest, Transport},
};

/// Keep reading, or the stream said it is over (`[DONE]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Continue,
    End,
}

/// One dialect's stream accumulation.
pub(crate) trait StreamFold: Send + 'static {
    /// One SSE event: push the live events it produced; an error ends the stream.
    fn on_event(&mut self, ev: &SseEvent, out: &mut Vec<ProviderEvent>) -> Result<Step, String>;

    /// Close the message. `failure` is why the stream ended early, if it did.
    fn finish(self, failure: Option<&str>, cancel: &CancellationToken) -> AssistantMessage;
}

/// Everything a call needs besides the fold.
pub(crate) struct Call {
    pub transport: Arc<dyn Transport>,
    pub request: HttpRequest,
    pub cancel: CancellationToken,
    pub retry: RetryPolicy,
}

pub(crate) fn drive<F: StreamFold>(call: Call, mut fold: F) -> ProviderStream {
    Box::pin(async_stream::stream! {
        let Call { transport, request, cancel, retry } = call;
        let mut failure: Option<String> = None;
        match http::send(&*transport, &request, &cancel, &retry).await {
            Err(e) => failure = Some(e),
            Ok(resp) => {
                let mut body = resp.body;
                let mut parser = SseParser::default();
                let mut out = Vec::new();
                'read: loop {
                    let next = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            failure = Some("request aborted".into());
                            break 'read;
                        }
                        next = body.next() => next,
                    };
                    let (events, ended) = match next {
                        Some(Ok(bytes)) => (parser.push(&bytes), false),
                        Some(Err(e)) => {
                            failure = Some(e.message);
                            break 'read;
                        }
                        None => (parser.finish(), true),
                    };
                    for ev in &events {
                        match fold.on_event(ev, &mut out) {
                            Ok(step) => {
                                for e in out.drain(..) {
                                    yield e;
                                }
                                if step == Step::End {
                                    break 'read;
                                }
                            }
                            Err(e) => {
                                failure = Some(e);
                                break 'read;
                            }
                        }
                    }
                    if ended {
                        break 'read;
                    }
                }
            }
        }
        yield ProviderEvent::Done(fold.finish(failure.as_deref(), &cancel));
    })
}

/// An OpenAI-shaped event's JSON. Like the SDK, a payload with an `error` ends the stream.
pub(crate) fn openai_json(ev: &SseEvent) -> Result<Value, String> {
    let v: Value = serde_json::from_str(&ev.data)
        .map_err(|e| format!("could not parse a stream event as JSON: {e}"))?;
    openai_error(v)
}

/// The event itself, or the `error` it carries as the stream's failure.
pub(crate) fn openai_error(v: Value) -> Result<Value, String> {
    match v.get("error") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(v),
        Some(err) => Err(err
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| err.to_string(), str::to_string)),
    }
}

/// A JSON number as a token count (absent, negative or not a number = 0).
pub(crate) fn count(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).map_or(0, |n| n.max(0.0) as u64)
}

/// A non-empty JSON string.
pub(crate) fn text_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}
