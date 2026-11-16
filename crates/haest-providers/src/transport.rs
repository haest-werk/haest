//! How a model request travels. Every provider POSTs one JSON body and reads
//! the answer as a byte stream (server-sent events); nothing else about HTTP
//! reaches an adapter.
//!
//! PLUG POINT: implement [`Transport`] to route every model call somewhere
//! else -- a host's own gateway over a unix socket, a recorder, a fake for
//! tests. The default is [`ReqwestTransport`](crate::ReqwestTransport).

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// The response body, chunk by chunk as it arrives.
pub type ByteStream = BoxStream<'static, Result<Bytes, TransportError>>;

/// One POST of a JSON body.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl HttpRequest {
    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

/// What came back: the status and headers as soon as they arrive, the body as a stream.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ByteStream,
}

impl HttpResponse {
    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// The request never got an answer, or the body broke off.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub message: String,
    /// A connection that failed or timed out before any answer: worth another try.
    pub retryable: bool,
}

impl TransportError {
    /// No answer at all (refused, reset, timed out): retried.
    pub fn connect(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    /// Anything else: never retried.
    pub fn other(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// POST `req`. Returns once the status and headers are in; the body streams.
    /// Should give up promptly when `cancel` fires (the caller also stops waiting).
    async fn post(
        &self,
        req: HttpRequest,
        cancel: CancellationToken,
    ) -> Result<HttpResponse, TransportError>;
}
