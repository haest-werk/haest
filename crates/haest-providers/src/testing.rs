//! Fakes for tests, in this crate and every crate that builds on it
//! (`haest-providers = { ..., features = ["testing"] }` in dev-dependencies):
//! a [`FakeHttp`] that answers with recorded SSE bodies and records every
//! request, plus builders for those bodies in each dialect.
//!
//! ```
//! use haest_core::Provider;
//! use haest_providers::{
//!     CompatFlavor, CompatOptions, CompatProvider,
//!     testing::{FakeHttp, FakeReply, chat_sse, collect, request},
//! };
//! use serde_json::json;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let http = FakeHttp::new([FakeReply::sse(chat_sse(&[
//!     json!({"choices": [{"index": 0, "delta": {"content": "hi"}}]}),
//! ]))]);
//! let opts = CompatOptions::new("test", CompatFlavor::Generic, "key");
//! let provider = CompatProvider::new(opts, http.clone());
//!
//! let (_events, message) = collect(provider.stream(request("some-model"))).await;
//! assert_eq!(message.text(), "hi");
//! assert_eq!(http.last_body()["model"], "some-model");
//! # }
//! ```

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use futures::{StreamExt, stream};
use haest_core::{
    messages::{AssistantMessage, Message},
    provider::{ProviderEvent, ProviderRequest, ProviderStream},
};
use haest_http::{Body, Bytes, HttpClient, HttpError, Request, Response, async_trait};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

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
    pub fn then(mut self, then: FakeEnd) -> Self {
        if let FakeReply::Response { end, .. } = &mut self {
            *end = then;
        }
        self
    }

    /// Then add this response header (`retry-after`, `x-should-retry`, ...).
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let FakeReply::Response { headers, .. } = &mut self {
            headers.push((name.into(), value.into()));
        }
        self
    }
}

/// An HTTP client that answers from a script, in order, and records every
/// request. Out of script = a non-retryable error, so an extra call fails loudly.
#[derive(Default)]
pub struct FakeHttp {
    replies: Mutex<VecDeque<FakeReply>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeHttp {
    /// A client that gives `replies`, one per request, in order. Shared
    /// (`Arc`), so a test keeps a handle to read the requests after handing
    /// a clone to a provider.
    pub fn new(replies: impl IntoIterator<Item = FakeReply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::default(),
        })
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// The last request's JSON body (Null when there was none).
    pub fn last_body(&self) -> Value {
        self.requests
            .lock()
            .unwrap()
            .last()
            .map(Request::json)
            .unwrap_or(Value::Null)
    }
}

#[async_trait]
impl HttpClient for FakeHttp {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        self.requests.lock().unwrap().push(req);
        let reply = self.replies.lock().unwrap().pop_front();
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
    ProviderRequest {
        model: model.into(),
        system: "sys".into(),
        messages,
        tools: Vec::new(),
        cancel: CancellationToken::new(),
        effort: None,
        max_tokens: None,
        web_search: None,
    }
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
        })
        .collect()
}
