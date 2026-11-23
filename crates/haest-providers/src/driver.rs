//! The one stream loop every dialect shares: send the request (with retries),
//! cut the body into SSE events, hand each to the dialect's fold, and end with
//! exactly one `Done`. A failure anywhere -- refused request, broken body, an
//! error event, cancellation -- still ends with `Done`; the fold decides what
//! of the half-built message survives. A call answered with one JSON body
//! ([`drive_json`]) and one never sent ([`not_sent`]) end the same way.

use futures::StreamExt;
use haest_core::{
    error::report,
    messages::AssistantMessage,
    provider::{ProviderEvent, ProviderStream},
};
use haest_http::Request;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::{
    send::{self, Sender},
    sse::{SseEvent, SseParser},
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

/// Send `request` with `sender` and fold its stream; `cancel` is the request's.
pub(crate) fn drive<F: StreamFold>(
    sender: &Sender,
    request: Request,
    cancel: CancellationToken,
    mut fold: F,
) -> ProviderStream {
    let sender = sender.clone();
    Box::pin(async_stream::stream! {
        let mut failure: Option<String> = None;
        match sender.send(&request, &cancel).await {
            Err(e) => failure = Some(e),
            Ok(resp) => {
                let mut body = resp.body;
                let mut parser = SseParser::default();
                let mut out = Vec::new();
                'read: loop {
                    let next = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            failure = Some(send::ABORTED.into());
                            break 'read;
                        }
                        next = body.next() => next,
                    };
                    let (parsed, ended) = match next {
                        Some(Ok(bytes)) => (parser.push(&bytes), false),
                        Some(Err(e)) => {
                            failure = Some(report(&e));
                            break 'read;
                        }
                        None => (parser.finish(), true),
                    };
                    let events = match parsed {
                        Ok(events) => events,
                        Err(too_long) => {
                            failure = Some(too_long.to_string());
                            break 'read;
                        }
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

/// A fold that also reads a call answered with one JSON body instead of a
/// stream ([`drive_json`]).
pub(crate) trait JsonFold: StreamFold {
    /// The whole response body; an error fails the call.
    fn on_response(&mut self, body: Value) -> Result<(), String>;
}

/// The most a JSON answer's body is read: far more than a message holds.
const JSON_BODY_CAP: usize = 16 * 1024 * 1024;

/// [`drive`] for a call that does not stream (Anthropic's cache pre-warm):
/// send `request` with `sender`, read its whole body, hand it to `fold`, and
/// end with exactly one `Done`.
pub(crate) fn drive_json<F: JsonFold>(
    sender: &Sender,
    request: Request,
    cancel: CancellationToken,
    mut fold: F,
) -> ProviderStream {
    let sender = sender.clone();
    Box::pin(futures::stream::once(async move {
        let failure = match sender.send(&request, &cancel).await {
            Err(e) => Some(e),
            Ok(resp) => match send::read_body(resp, JSON_BODY_CAP, &cancel).await {
                (_, Some(short)) => Some(short),
                (body, None) => serde_json::from_slice(&body)
                    .map_err(|e| format!("could not parse the response as JSON: {e}"))
                    .and_then(|body| fold.on_response(body))
                    .err(),
            },
        };
        ProviderEvent::Done(fold.finish(failure.as_deref(), &cancel))
    }))
}

/// A call that is not sent: its stream is one `Done`, failed for `why`
/// (or aborted, if `cancel` already fired).
pub(crate) fn not_sent<F: StreamFold>(
    fold: F,
    why: &str,
    cancel: &CancellationToken,
) -> ProviderStream {
    let done = ProviderEvent::Done(fold.finish(Some(why), cancel));
    Box::pin(futures::stream::iter([done]))
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

/// `v`'s string field `key`, or "" when it has none.
pub(crate) fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Streamed tool arguments (JSON text) -> an input object; blank text is no
/// arguments. Both OpenAI dialects stream arguments the same way.
pub(crate) fn parse_args(args: &str) -> Map<String, Value> {
    if args.trim().is_empty() {
        return Map::new();
    }
    parse_object(args)
}

/// Never fails: a non-object is `{ value }`, unparseable text `{ _unparsed }`,
/// which the loop answers as a model error.
pub(crate) fn parse_object(text: &str) -> Map<String, Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => m,
        Ok(other) => Map::from_iter([("value".to_string(), other)]),
        Err(_) => Map::from_iter([("_unparsed".to_string(), Value::String(text.to_string()))]),
    }
}
