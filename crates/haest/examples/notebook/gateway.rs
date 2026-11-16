//! The notebook's model gateway, faked: the node answers `/agent/llm/v1/chat/completions`
//! on its unix socket and holds the keys; the run holds none. Here it answers a
//! scripted conversation in OpenAI-compatible SSE and keeps every request.
//!
//! PLUG POINT: `haest::http::HttpClient`.

use std::{collections::VecDeque, sync::Mutex};

use haest::http::{Body, Bytes, HttpClient, HttpError, Request, Response, async_trait};
use serde_json::{Value, json};

/// Where the gateway listens, as the provider's base URL. The client sends
/// the path over the socket; the host part means nothing.
pub const BASE_URL: &str = "http://files.sock/agent/llm/v1";

#[derive(Default)]
pub struct Gateway {
    replies: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<Request>>,
}

impl Gateway {
    /// The model's next replies, in order. Out of script = a failed call.
    pub fn script(&self, replies: impl IntoIterator<Item = String>) {
        *self.replies.lock().unwrap() = replies.into_iter().collect();
    }

    /// Every request since the last call.
    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

#[async_trait]
impl HttpClient for Gateway {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        self.requests.lock().unwrap().push(req);
        let Some(body) = self.replies.lock().unwrap().pop_front() else {
            return Err(HttpError::Request("gateway: no scripted reply left".into()));
        };
        // The socket hands the body over in pieces; SSE events straddle them.
        let chunks: Vec<Result<Bytes, HttpError>> = body
            .as_bytes()
            .chunks(16)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let body = Body::from_stream(futures::stream::iter(chunks));
        Ok(Response::new(200, body).with_header("content-type", "text/event-stream"))
    }
}

/// A tool call the scripted model makes: id, tool name, input.
pub struct Call(pub &'static str, pub &'static str, pub Value);

/// What every scripted model call costs, as OpenRouter reports it in the usage.
pub const CALL_USD: f64 = 0.001;

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
        "usage": {"prompt_tokens": 900, "completion_tokens": 60, "cost": CALL_USD}
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
