//! Fakes for tests, in this crate and every crate that builds on it
//! (`haest-providers = { ..., features = ["testing"] }` in dev-dependencies):
//! a [`FakeHttp`] that answers with recorded SSE bodies and records every
//! request, plus builders for those bodies in each dialect.
//!
//! ```
//! use haest_core::Provider;
//! use haest_providers::{
//!     CompatOptions, CompatProvider,
//!     testing::{FakeHttp, FakeReply, chat_sse, collect, request},
//! };
//! use serde_json::json;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let http = FakeHttp::new([FakeReply::sse(chat_sse(&[
//!     json!({"choices": [{"index": 0, "delta": {"content": "hi"}}]}),
//! ]))]);
//! let opts = CompatOptions::new("test", "key");
//! let provider = CompatProvider::new(opts, http.clone());
//!
//! let (_events, message) = collect(provider.stream(request("some-model"))).await;
//! assert_eq!(message.text(), "hi");
//! assert_eq!(http.last_body()["model"], "some-model");
//! # }
//! ```

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use futures::{StreamExt, stream};
use haest_core::{
    messages::{AssistantMessage, Message},
    provider::{ProviderEvent, ProviderRequest, ProviderStream},
};
use haest_http::{Body, Bytes, HttpClient, HttpError, Request, Response, async_trait};
use serde_json::Value;

/// What happens after a reply's chunks.
#[derive(Debug, Clone, PartialEq)]
pub enum FakeEnd {
    /// The body ends cleanly.
    Close,
    /// The body breaks off with this error.
    Fail(String),
    /// The body never ends: only cancellation gets the caller out.
    Hang,
}

/// One scripted answer.
#[derive(Debug)]
pub enum FakeReply {
    /// An answer: the status and headers, then the body in these chunks, then `end`.
    Response {
        /// The HTTP status.
        status: u16,
        /// The response headers.
        headers: Vec<(String, String)>,
        /// The body, one chunk per item, as the client hands it over.
        chunks: Vec<Bytes>,
        /// How the body ends after the last chunk.
        end: FakeEnd,
    },
    /// No answer at all.
    Error(HttpError),
}

/// Bytes per chunk for SSE bodies: small and odd, so every test also crosses chunk boundaries.
const CHUNK: usize = 7;

impl FakeReply {
    /// A 200 streaming `body`, cut into small chunks.
    pub fn sse(body: impl Into<String>) -> Self {
        let body: String = body.into();
        let chunks = body
            .as_bytes()
            .chunks(CHUNK)
            .map(Bytes::copy_from_slice)
            .collect();
        FakeReply::Response {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks,
            end: FakeEnd::Close,
        }
    }

    /// A refusal: `status` with `body`.
    pub fn status(status: u16, body: impl Into<String>) -> Self {
        FakeReply::Response {
            status,
            headers: Vec::new(),
            chunks: vec![Bytes::from(body.into())],
            end: FakeEnd::Close,
        }
    }

    /// Then end the body this way instead of cleanly.
    #[must_use]
    pub fn then(mut self, then: FakeEnd) -> Self {
        if let FakeReply::Response { end, .. } = &mut self {
            *end = then;
        }
        self
    }

    /// Then add this response header (`retry-after`, `x-should-retry`, ...),
    /// as [`Response::with_header`](haest_http::Response::with_header) takes it.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        if let FakeReply::Response { headers, .. } = &mut self {
            headers.push((name.into(), value.into()));
        }
        self
    }
}

/// An HTTP client that answers from a script, in order, and records every
/// request. Out of script = a non-retryable error, so an extra call fails loudly.
///
/// Cloning is cheap and the clones share the script and the record: a test
/// hands a clone to a provider and reads the requests on its own.
#[derive(Clone, Default)]
pub struct FakeHttp(Arc<Script>);

#[derive(Default)]
struct Script {
    replies: Mutex<VecDeque<FakeReply>>,
    requests: Mutex<Vec<Request>>,
}

impl Script {
    fn lock_replies(&self) -> MutexGuard<'_, VecDeque<FakeReply>> {
        self.replies
            .lock()
            .expect("the fake's replies lock poisoned")
    }

    fn lock_requests(&self) -> MutexGuard<'_, Vec<Request>> {
        self.requests
            .lock()
            .expect("the fake's requests lock poisoned")
    }
}

impl FakeHttp {
    /// A client that gives `replies`, one per request, in order.
    pub fn new(replies: impl IntoIterator<Item = FakeReply>) -> Self {
        Self(Arc::new(Script {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::default(),
        }))
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<Request> {
        self.0.lock_requests().clone()
    }

    /// The last request's JSON body (Null when there was none).
    pub fn last_body(&self) -> Value {
        self.0
            .lock_requests()
            .last()
            .map(Request::json_body)
            .unwrap_or(Value::Null)
    }
}

/// How many replies are left and how many requests came; not their content.
impl fmt::Debug for FakeHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let left = self.0.lock_replies().len();
        let sent = self.0.lock_requests().len();
        f.debug_struct("FakeHttp")
            .field("replies_left", &left)
            .field("requests", &sent)
            .finish()
    }
}

#[async_trait]
impl HttpClient for FakeHttp {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        self.0.lock_requests().push(req);
        let reply = self.0.lock_replies().pop_front();
        let (status, headers, chunks, end) = match reply {
            None => return Err(HttpError::Request("fake HTTP: out of replies".into())),
            Some(FakeReply::Error(e)) => return Err(e),
            Some(FakeReply::Response {
                status,
                headers,
                chunks,
                end,
            }) => (status, headers, chunks, end),
        };
        let head = stream::iter(chunks.into_iter().map(Ok));
        let body = match end {
            FakeEnd::Close => Body::from_stream(head),
            FakeEnd::Fail(msg) => Body::from_stream(head.chain(stream::once(async move {
                Err(HttpError::Body(msg.into()))
            }))),
            FakeEnd::Hang => Body::from_stream(head.chain(stream::pending())),
        };
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// One SSE event; `event` None sends only the `data:` line.
fn sse_event(event: Option<&str>, data: &Value) -> String {
    match event {
        Some(name) => format!("event: {name}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

/// A body of events named after their own `type` (Anthropic Messages, OpenAI Responses).
pub fn typed_sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| sse_event(e.get("type").and_then(Value::as_str), e))
        .collect()
}

/// A Chat Completions body: one `data:` per chunk, then `[DONE]`.
pub fn chat_sse(chunks: &[Value]) -> String {
    let mut body: String = chunks.iter().map(|c| sse_event(None, c)).collect();
    body.push_str("data: [DONE]\n\n");
    body
}

/// A request for `model` with a one-line user turn.
pub fn request(model: &str) -> ProviderRequest {
    request_with(model, vec![Message::user("hi")])
}

/// A request for `model` with these messages: system prompt `sys`, no tools,
/// no effort, no token cap, no web search, a fresh cancellation token.
pub fn request_with(model: &str, messages: Vec<Message>) -> ProviderRequest {
    ProviderRequest::new(model, messages).with_system("sys")
}

/// Drain a provider stream: every event, and the message `Done` carried.
///
/// # Panics
///
/// When the stream does not end with exactly one `Done`, the contract every
/// provider keeps.
pub async fn collect(stream: ProviderStream) -> (Vec<ProviderEvent>, AssistantMessage) {
    let events: Vec<ProviderEvent> = stream.collect().await;
    let Some(ProviderEvent::Done(msg)) = events.last().cloned() else {
        panic!("stream did not end with Done: {events:?}");
    };
    let dones = events
        .iter()
        .filter(|e| matches!(e, ProviderEvent::Done(_)))
        .count();
    assert_eq!(dones, 1, "exactly one Done");
    (events, msg)
}

/// A short name per event, for asserting the order of a stream.
pub fn kinds(events: &[ProviderEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            ProviderEvent::TextDelta(_) => "text_delta",
            ProviderEvent::ThinkingDelta(_) => "thinking_delta",
            ProviderEvent::ToolCallStart { .. } => "tool_call_start",
            ProviderEvent::Done(_) => "done",
            _ => "other",
        })
        .collect()
}
