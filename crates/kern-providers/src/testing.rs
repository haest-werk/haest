//! Fakes for tests, in this crate and every crate that builds on it
//! (`kern-providers = { ..., features = ["testing"] }` in dev-dependencies):
//! a [`FakeTransport`] that answers with recorded SSE bodies and records every
//! request, plus builders for those bodies in each dialect.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, stream};
use kern_core::{
    messages::{AssistantMessage, Message},
    provider::{ProviderEvent, ProviderRequest, ProviderStream},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpRequest, HttpResponse, Transport, TransportError};

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
#[derive(Debug, Clone)]
pub enum FakeReply {
    Response {
        status: u16,
        headers: Vec<(String, String)>,
        chunks: Vec<Bytes>,
        end: FakeEnd,
    },
    /// No answer at all.
    Error(TransportError),
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
    pub fn then(self, end: FakeEnd) -> Self {
        match self {
            FakeReply::Response {
                status,
                headers,
                chunks,
                ..
            } => FakeReply::Response {
                status,
                headers,
                chunks,
                end,
            },
            other => other,
        }
    }

    pub fn header(self, name: &str, value: &str) -> Self {
        match self {
            FakeReply::Response {
                status,
                mut headers,
                chunks,
                end,
            } => {
                headers.push((name.into(), value.into()));
                FakeReply::Response {
                    status,
                    headers,
                    chunks,
                    end,
                }
            }
            other => other,
        }
    }
}

/// A transport that answers from a script, in order, and records every request.
/// Out of script = a non-retryable error, so an extra call fails loudly.
#[derive(Default)]
pub struct FakeTransport {
    replies: Mutex<VecDeque<FakeReply>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeTransport {
    pub fn new(replies: impl IntoIterator<Item = FakeReply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::default(),
        })
    }

    pub fn push(&self, reply: FakeReply) {
        self.replies.lock().unwrap().push_back(reply);
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// The last request's body.
    pub fn last_body(&self) -> Value {
        self.requests
            .lock()
            .unwrap()
            .last()
            .map(|r| r.body.clone())
            .unwrap_or(Value::Null)
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn post(
        &self,
        req: HttpRequest,
        _cancel: CancellationToken,
    ) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(req);
        let reply = self.replies.lock().unwrap().pop_front();
        let Some(reply) = reply else {
            return Err(TransportError::other("fake transport: out of replies"));
        };
        let (status, headers, chunks, end) = match reply {
            FakeReply::Error(e) => return Err(e),
            FakeReply::Response {
                status,
                headers,
                chunks,
                end,
            } => (status, headers, chunks, end),
        };
        let head = stream::iter(chunks.into_iter().map(Ok));
        let body = match end {
            FakeEnd::Close => head.boxed(),
            FakeEnd::Fail(msg) => head
                .chain(stream::once(async move { Err(TransportError::other(msg)) }))
                .boxed(),
            FakeEnd::Hang => head.chain(stream::pending()).boxed(),
        };
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// One SSE event; `event` None sends only the `data:` line.
pub fn sse_event(event: Option<&str>, data: &Value) -> String {
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
