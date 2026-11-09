//! Sending one model request: retries the way the official SDKs do (a
//! connection failure, 408, 409, 429 or 5xx, up to three more tries with
//! jittered exponential backoff, honouring `retry-after` and `x-should-retry`),
//! and turns a refused request into the message the human sees.
//!
//! Only the request is retried. Once the stream has started, a failure ends the
//! message with `StopReason::Error`: a retry would replay text already shown.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    time::Duration,
};

use futures::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpRequest, HttpResponse, Transport};

#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Tries after the first one.
    pub max_retries: u32,
    /// The first backoff; doubles per retry.
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    /// The SDKs' policy: 3 retries, 0.5 s doubling to at most 8 s.
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// Never retry.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Backoff before retry number `retry` (0-based): doubling, capped, minus up to 25% jitter.
    fn backoff(&self, retry: u32) -> Duration {
        let doubled = self.initial_delay.saturating_mul(1u32 << retry.min(16));
        let capped = doubled.min(self.max_delay);
        capped.mul_f64(1.0 - random_unit() * 0.25)
    }
}

/// A uniform-ish number in [0, 1) without a random-number dependency.
fn random_unit() -> f64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u64(0);
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// `n` random lowercase hex digits (a stand-in id for a tool call the server sent without one).
pub(crate) fn random_hex(n: usize) -> String {
    let mut out = String::new();
    while out.len() < n {
        let mut h = RandomState::new().build_hasher();
        h.write_usize(out.len());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(n);
    out
}

/// The OpenAI dialects' headers: a bearer key and JSON, then `extra` (which wins on a clash).
pub(crate) fn bearer_headers(api_key: &str, extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut headers = vec![
        ("authorization".to_string(), format!("Bearer {api_key}")),
        ("content-type".to_string(), "application/json".to_string()),
        ("accept".to_string(), "application/json".to_string()),
    ];
    for (name, value) in extra {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
    headers
}

fn should_retry(resp: &HttpResponse) -> bool {
    match resp.header("x-should-retry") {
        Some("true") => true,
        Some("false") => false,
        _ => matches!(resp.status, 408 | 409 | 429) || resp.status >= 500,
    }
}

/// The server's own wait, when it names one under a minute.
fn retry_after(resp: &HttpResponse) -> Option<Duration> {
    let ms = resp
        .header("retry-after-ms")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .or_else(|| {
            resp.header("retry-after")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map(|s| s * 1000.0)
        })?;
    (0.0..60_000.0)
        .contains(&ms)
        .then(|| Duration::from_secs_f64(ms / 1000.0))
}

async fn sleep(delay: Duration, cancel: &CancellationToken) -> Result<(), String> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("request aborted".into()),
        _ = tokio::time::sleep(delay) => Ok(()),
    }
}

/// POST until the server answers 2xx, a refusal is final, or the retries run out.
/// The error is the message for `AssistantMessage::error_message`.
pub(crate) async fn send(
    transport: &dyn Transport,
    req: &HttpRequest,
    cancel: &CancellationToken,
    retry: &RetryPolicy,
) -> Result<HttpResponse, String> {
    let mut attempt = 0;
    loop {
        if cancel.is_cancelled() {
            return Err("request aborted".into());
        }
        let sent = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("request aborted".into()),
            sent = transport.post(req.clone(), cancel.clone()) => sent,
        };
        let retries_left = attempt < retry.max_retries;
        match sent {
            Ok(resp) if resp.is_success() => return Ok(resp),
            Ok(resp) if retries_left && should_retry(&resp) => {
                let delay = retry_after(&resp).unwrap_or_else(|| retry.backoff(attempt));
                drop(resp);
                sleep(delay, cancel).await?;
            }
            Ok(resp) => return Err(refusal_message(resp, cancel).await),
            Err(e) if retries_left && e.retryable => sleep(retry.backoff(attempt), cancel).await?,
            Err(e) => return Err(e.message),
        }
        attempt += 1;
    }
}

/// The most an error body is read for its message.
const ERROR_BODY_CAP: usize = 64 * 1024;

/// `"<status> <the API's own message>"`, like the SDKs say it.
async fn refusal_message(resp: HttpResponse, cancel: &CancellationToken) -> String {
    let status = resp.status;
    let mut body = Vec::new();
    let mut stream = resp.body;
    while body.len() < ERROR_BODY_CAP {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            next = stream.next() => next,
        };
        match next {
            Some(Ok(chunk)) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    let text = String::from_utf8_lossy(&body);
    match error_detail(text.trim()) {
        Some(detail) => format!("{status} {detail}"),
        None => format!("{status} status code (no body)"),
    }
}

/// `error.message`, else a top-level `message`, else the body as sent.
fn error_detail(body: &str) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let json: Option<Value> = serde_json::from_str(body).ok();
    let message = json.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
    });
    Some(message.map_or_else(|| body.chars().take(2000).collect(), str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap_with_at_most_a_quarter_off() {
        let p = RetryPolicy::default();
        for (retry, full) in [(0, 500), (1, 1000), (2, 2000), (5, 8000), (30, 8000)] {
            let d = p.backoff(retry).as_millis() as u64;
            assert!(d <= full && d >= full * 3 / 4, "retry {retry}: {d}ms");
        }
    }

    #[test]
    fn the_api_message_is_preferred_over_the_raw_body() {
        let openai = r#"{"error":{"message":"bad model","type":"invalid_request_error"}}"#;
        assert_eq!(error_detail(openai).as_deref(), Some("bad model"));
        assert_eq!(error_detail(r#"{"message":"top"}"#).as_deref(), Some("top"));
        assert_eq!(error_detail("plain").as_deref(), Some("plain"));
        assert_eq!(error_detail(""), None);
    }

    #[test]
    fn random_hex_has_the_asked_length_and_alphabet() {
        let id = random_hex(8);
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
