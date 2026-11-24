//! Sending one model request: retries the way the official SDKs do
//! ([`RetryPolicy`]), and turns a refused request into the message the human
//! sees. Also the request shape every dialect shares: the URL, the headers,
//! a JSON POST.
//!
//! Only the request is retried. Once the stream has started, a failure ends the
//! message with `StopReason::Error`: a retry would replay text already shown.

mod shell;

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    time::Duration,
};

use futures::StreamExt;
use haest_core::error::report;
use haest_http::{
    __private::{authority, join_path},
    HttpClientRef, HttpError, Request, Response, check_endpoint_url,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::retry::{RetryPolicy, retry_after, should_retry};

pub(crate) use shell::{endpoint_options, sender_provider};

/// How long one try of a model call waits for its status and headers unless
/// a provider or the registry is told otherwise (`with_headers_timeout`;
/// haest's `[timeouts] model_headers`): ten minutes, as long as the SDKs
/// allow.
pub const HEADERS_TIMEOUT: Duration = Duration::from_secs(600);

/// How a provider sends its requests: the HTTP client, the retry policy, and
/// how long one try waits for its status and headers. Every provider holds
/// one; `with_retry` and `with_headers_timeout` set it.
#[derive(Debug, Clone)]
pub(crate) struct Sender {
    pub(crate) http: HttpClientRef,
    pub(crate) retry: RetryPolicy,
    pub(crate) headers_timeout: Duration,
}

impl Sender {
    /// Over `http`, with the default [`RetryPolicy`] and [`HEADERS_TIMEOUT`].
    pub(crate) fn new(http: HttpClientRef) -> Self {
        Self {
            http,
            retry: RetryPolicy::default(),
            headers_timeout: HEADERS_TIMEOUT,
        }
    }

    /// POST until the server answers 2xx, a refusal is final, or the retries
    /// run out. The error is the message for `AssistantMessage::error_message`.
    ///
    /// A URL with an `@` past its host, or with no host, is never sent to
    /// ([`check_endpoint_url`]): a base URL whose password holds an
    /// unescaped `/`, `?` or `#` reads to a URL parser as another host, and
    /// so does the first part of the path of a URL with no host.
    pub(crate) async fn send(
        &self,
        req: &Request,
        cancel: &CancellationToken,
    ) -> Result<Response, String> {
        check_endpoint_url(&req.url).map_err(|refused| refused.to_string())?;
        let retry = &self.retry;
        let mut attempt = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(ABORTED.into());
            }
            let sent = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ABORTED.into()),
                sent = self.try_once(req) => sent,
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
                Err(e) => return Err(report(&e)),
            }
            attempt += 1;
        }
    }

    /// One try: the answer's status and headers, or no answer within
    /// `headers_timeout` (dropping the request, which cancels it).
    async fn try_once(&self, req: &Request) -> Result<Response, HttpError> {
        let wait = self.headers_timeout;
        tokio::time::timeout(wait, self.http.send(req.clone()))
            .await
            .unwrap_or(Err(HttpError::Timeout(wait)))
    }
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

/// The most an error body is read for its message.
const ERROR_BODY_CAP: usize = 64 * 1024;

/// `resp`'s body read to its end, and None; or, when it stopped short, why:
/// it broke off, `cancel` fired, or it ran past `cap` bytes. The bytes read
/// come back either way.
pub(crate) async fn read_body(
    resp: Response,
    cap: usize,
    cancel: &CancellationToken,
) -> (Vec<u8>, Option<String>) {
    let mut body = Vec::new();
    let mut stream = resp.body;
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return (body, Some(ABORTED.into())),
            next = stream.next() => next,
        };
        match next {
            None => return (body, None),
            Some(Err(e)) => return (body, Some(report(&e))),
            Some(Ok(chunk)) => body.extend_from_slice(&chunk),
        }
        if body.len() > cap {
            return (body, Some(format!("the response ran past {cap} bytes")));
        }
    }
}

/// `"<status> <the API's own message>"`, like the SDKs say it.
async fn refusal_message(resp: Response, cancel: &CancellationToken) -> String {
    let status = resp.status;
    // A body cut short still says what it said so far.
    let (body, _) = read_body(resp, ERROR_BODY_CAP, cancel).await;
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

/// Whether an OpenAI dialect with `base_url` reaches OpenAI's own API: none
/// set ([`OPENAI_URL`]), or a host of `api.openai.com` or under it (a
/// regional one such as `eu.api.openai.com`). Only there does a field that
/// only OpenAI documents (`prompt_cache_key`, from a request's partition key)
/// go out; an unknown server may refuse a field it does not know.
pub(crate) fn is_openai_api(base_url: Option<&str>) -> bool {
    let Some(url) = base_url else { return true };
    let authority = &url[authority(url)];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host_port.split(':').next().unwrap_or(host_port);
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "api.openai.com" || host.ends_with(".api.openai.com")
}

/// `path` under `base` (None = `default`), however many slashes its path
/// ends in, and before its query: a gateway's key or an `api-version` there
/// stays the query (haest-http's `join_path`).
pub(crate) fn endpoint(base: Option<&str>, default: &str, path: &str) -> String {
    join_path(base.unwrap_or(default), path)
}

/// A model call to one of the OpenAI dialects: `body` POSTed as JSON to `url`
/// with a bearer key, then the configured `extra` headers ([`configured`]).
pub(crate) fn bearer_post(
    url: String,
    api_key: &str,
    body: &Value,
    extra: &[(String, String)],
) -> Request {
    let req = Request::post(url)
        .with_header("authorization", format!("Bearer {api_key}"))
        .with_json(body)
        .with_header("accept", "application/json");
    configured(req, extra)
}

/// `req` with the configured `extra` headers over the dialect's own: one of
/// the same name, in any case, replaces it ([`Request::with_header`]; a
/// header sent twice reaches the server as two values, and one that reads the
/// first never sees the configured one).
pub(crate) fn configured(req: Request, extra: &[(String, String)]) -> Request {
    extra
        .iter()
        .fold(req, |req, (name, value)| req.with_header(name, value))
}

/// What an options type's Debug output shows: a key as `<redacted>`, the
/// configured headers by name only (a proxy's key rides in one), a base URL
/// without its `user:password@`.
pub(crate) use haest_http::__private::{HeaderNames, REDACTED, UrlDebug};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_message_is_preferred_over_the_raw_body() {
        let openai = r#"{"error":{"message":"bad model","type":"invalid_request_error"}}"#;
        assert_eq!(error_detail(openai).as_deref(), Some("bad model"));
        assert_eq!(error_detail(r#"{"message":"top"}"#).as_deref(), Some("top"));
        assert_eq!(error_detail("plain").as_deref(), Some("plain"));
        assert_eq!(error_detail(""), None);
    }

    #[test]
    fn only_openai_hosts_count_as_openais_api() {
        for ours in [
            None,
            Some("https://api.openai.com/v1"),
            Some("https://eu.api.openai.com/v1/"),
            Some("https://API.OpenAI.com:443/v1?x=1"),
        ] {
            assert!(is_openai_api(ours), "{ours:?}");
        }
        for theirs in [
            "https://openrouter.ai/api/v1",
            "http://localhost:1234/v1",
            "https://api.openai.com.evil.example/v1",
            "https://evil.example/api.openai.com/v1",
            "https://api.openai.com@evil.example/v1",
        ] {
            assert!(!is_openai_api(Some(theirs)), "{theirs}");
        }
    }

    #[test]
    fn random_hex_has_the_asked_length_and_alphabet() {
        let id = random_hex(8);
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
