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
use haest_http::{Bytes, HttpClient, HttpError, Method, Request, Response};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// How a provider retries a request that got no answer or was refused, the way
/// the official SDKs do.
///
/// A try is repeated after no answer (a connection failure, or no status and
/// headers within `headers_timeout`: [`HttpError::is_retryable`]), or a 408,
/// 409, 429 or 5xx answer. The server has the last word: an
/// `x-should-retry: false` header stops the retries, `true` retries any
/// status. The wait is the server's `retry-after-ms` or `retry-after` when it
/// names one under a minute, else `initial_delay` doubled per retry up to
/// `max_delay`, minus up to 25% jitter. Cancelling the request ends the wait.
///
/// Only the request is retried. Once the answer streams, a failure ends the
/// message: a retry would replay text the human has already seen.
///
/// Every provider takes one through `with_retry`, and
/// [`RegistryOptions::retry`](crate::RegistryOptions::retry) sets it for all.
///
/// ```
/// use std::time::Duration;
///
/// use haest_providers::RetryPolicy;
///
/// let patient = RetryPolicy { max_retries: 6, max_delay: Duration::from_secs(30), ..RetryPolicy::default() };
/// assert_eq!(patient.initial_delay, Duration::from_millis(500));
/// assert_eq!(RetryPolicy::none().max_retries, 0);
/// ```
///
/// [`HttpError::is_retryable`]: haest_http::HttpError::is_retryable
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Tries after the first one; 0 never retries.
    pub max_retries: u32,
    /// The first backoff; doubles per retry.
    pub initial_delay: Duration,
    /// The longest backoff. A server's own `retry-after` may ask for longer, up to a minute.
    pub max_delay: Duration,
    /// How long one try waits for the status and headers before it counts as
    /// unanswered; the body then takes as long as it takes. A long prompt can
    /// sit in a queue before the first byte.
    pub headers_timeout: Duration,
}

impl Default for RetryPolicy {
    /// The SDKs' policy: 3 retries, 0.5 s doubling to at most 8 s, and ten
    /// minutes for each try's status, as long as the SDKs allow.
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
            headers_timeout: Duration::from_secs(600),
        }
    }
}

impl RetryPolicy {
    /// Never retry: the first refusal or connection failure is the answer.
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

/// OpenAI's own API: where both OpenAI dialects go when no base URL is set.
pub(crate) const OPENAI_URL: &str = "https://api.openai.com/v1";

/// `path` under `base` (None = `default`), however many slashes `base` ends in.
pub(crate) fn endpoint(base: &Option<String>, default: &str, path: &str) -> String {
    let base = base.as_deref().unwrap_or(default);
    format!("{}/{path}", base.trim_end_matches('/'))
}

/// The OpenAI dialects' headers: a bearer key and JSON, then `extra` (which wins on a clash).
pub(crate) fn bearer_headers(api_key: &str, extra: &[(String, String)]) -> Vec<(String, String)> {
    let defaults = [
        ("authorization", format!("Bearer {api_key}")),
        ("content-type", "application/json".to_string()),
        ("accept", "application/json".to_string()),
    ];
    with_headers(defaults, extra)
}

/// A dialect's `defaults`, then the configured `extra`: one of the same name,
/// in any case, replaces the default (a header sent twice reaches the server
/// as two values, and one that reads the first never sees the configured one).
pub(crate) fn with_headers<const N: usize>(
    defaults: [(&str, String); N],
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = defaults
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    for (name, value) in extra {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
    headers
}

/// A model call: `body` POSTed as JSON to `url` with `headers` (which carry
/// the key and the content type).
pub(crate) fn json_post(url: String, headers: Vec<(String, String)>, body: &Value) -> Request {
    Request {
        method: Method::Post,
        url,
        headers,
        body: Bytes::from(body.to_string()),
    }
}

fn should_retry(resp: &Response) -> bool {
    match resp.header("x-should-retry") {
        Some("true") => true,
        Some("false") => false,
        _ => matches!(resp.status, 408 | 409 | 429) || resp.status >= 500,
    }
}

/// The server's own wait, when it names one under a minute.
fn retry_after(resp: &Response) -> Option<Duration> {
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

/// Why a request or its stream ended early when its cancel token fired. The
/// message is marked aborted and this text is never shown (haest-core's
/// `mark_stream_failed`); it only makes the end a failure.
pub(crate) const ABORTED: &str = "request aborted";

async fn sleep(delay: Duration, cancel: &CancellationToken) -> Result<(), String> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(ABORTED.into()),
        _ = tokio::time::sleep(delay) => Ok(()),
    }
}

/// One try: the answer's status and headers, or no answer within
/// `retry.headers_timeout` (dropping the request, which cancels it).
async fn try_once(
    http: &dyn HttpClient,
    req: &Request,
    retry: &RetryPolicy,
) -> Result<Response, HttpError> {
    tokio::time::timeout(retry.headers_timeout, http.send(req.clone()))
        .await
        .unwrap_or(Err(HttpError::Timeout(retry.headers_timeout)))
}

/// POST until the server answers 2xx, a refusal is final, or the retries run out.
/// The error is the message for `AssistantMessage::error_message`.
pub(crate) async fn send(
    http: &dyn HttpClient,
    req: &Request,
    cancel: &CancellationToken,
    retry: &RetryPolicy,
) -> Result<Response, String> {
    let mut attempt = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(ABORTED.into());
        }
        let sent = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ABORTED.into()),
            sent = try_once(http, req, retry) => sent,
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
            Err(e) if retries_left && e.is_retryable() => {
                sleep(retry.backoff(attempt), cancel).await?
            }
            Err(e) => return Err(e.report()),
        }
        attempt += 1;
    }
}

/// The most an error body is read for its message.
const ERROR_BODY_CAP: usize = 64 * 1024;

/// `"<status> <the API's own message>"`, like the SDKs say it.
async fn refusal_message(resp: Response, cancel: &CancellationToken) -> String {
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
