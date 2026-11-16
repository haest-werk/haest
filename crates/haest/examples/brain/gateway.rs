//! brain's model gateway, faked: the node answers `/agent/llm/v1/chat/completions`
//! on its unix socket and holds the keys; the run holds none. Here it answers a
//! scripted conversation in OpenAI-compatible SSE and keeps every request.
//!
//! PLUG POINT: `haest::providers::Transport`.

use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, stream};
use haest::providers::{HttpRequest, HttpResponse, Transport, TransportError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Where the gateway listens, as the provider's base URL. The transport sends
/// the path over the socket; the host part means nothing.
pub const BASE_URL: &str = "http://files.sock/agent/llm/v1";

#[derive(Default)]
pub struct Gateway {
    replies: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl Gateway {
    /// The model's next replies, in order. Out of script = a failed call.
    pub fn script(&self, replies: impl IntoIterator<Item = String>) {
        *self.replies.lock().unwrap() = replies.into_iter().collect();
    }

    /// Every request since the last call.
    pub fn take_requests(&self) -> Vec<HttpRequest> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

#[async_trait]
impl Transport for Gateway {
    async fn post(
        &self,
        req: HttpRequest,
        _cancel: CancellationToken,
    ) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(req);
        let Some(body) = self.replies.lock().unwrap().pop_front() else {
            return Err(TransportError::other("gateway: no scripted reply left"));
        };
        // The socket hands the body over in pieces; SSE events straddle them.
        let chunks: Vec<Result<Bytes, TransportError>> = body
            .as_bytes()
            .chunks(16)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: stream::iter(chunks).boxed(),
        })
    }
}

/// A tool call the scripted model makes: id, tool name, input.
pub struct Call(pub &'static str, pub &'static str, pub Value);

/// One model reply in Chat Completions SSE: `text` streamed, then each call
/// (id and name first, the arguments in a later fragment, the way servers
/// stream them), the finish reason, and a usage chunk with OpenRouter's cost.
pub fn reply(text: &str, calls: &[Call]) -> String {
    let mut chunks = Vec::new();
    if !text.is_empty() {
        chunks.push(delta(json!({"content": text})));
    }
    for (i, Call(id, name, input)) in calls.iter().enumerate() {
        chunks.push(delta(json!({"tool_calls": [
            {"index": i, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}
        ]})));
        chunks.push(delta(json!({"tool_calls": [
            {"index": i, "function": {"arguments": input.to_string()}}
        ]})));
    }
    let finish = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    chunks.push(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}));
    chunks.push(json!({
        "choices": [],
        "usage": {"prompt_tokens": 900, "completion_tokens": 60, "cost": 0.001}
    }));
    let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    body
}

fn delta(d: Value) -> Value {
    json!({"choices": [{"index": 0, "delta": d}]})
}

/// The tool names a request offered the model, in order.
pub fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.pointer("/function/name")?.as_str().map(String::from))
        .collect()
}

/// The JSON Schema a request sent for one tool.
pub fn tool_schema<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t.pointer("/function/name").and_then(Value::as_str) == Some(name))
        .and_then(|t| t.pointer("/function/parameters"))
        .unwrap_or_else(|| panic!("no {name} in the request's tools"))
}
