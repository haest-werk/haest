//! How a model request travels. Every provider POSTs one JSON body
//! ([`HttpRequest`]) and reads the answer as a byte stream of server-sent
//! events ([`HttpResponse`]); nothing else about HTTP reaches an adapter.
//!
//! PLUG POINT: implement [`Transport`] to route every model call somewhere
//! else -- a host's own gateway over a unix socket, a recorder, a fake for
//! tests. The default is [`ReqwestTransport`](crate::ReqwestTransport). A
//! transport is handed to each provider's `new`, or to every provider at once
//! through [`RegistryOptions::transport`](crate::RegistryOptions::transport).

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// The response body, chunk by chunk as it arrives. Chunks may split an event,
/// a line or a UTF-8 character anywhere; an `Err` ends the body.
pub type ByteStream = BoxStream<'static, Result<Bytes, TransportError>>;

/// One POST of a JSON body: what a provider hands its [`Transport`].
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    /// The full URL, the API's path included (`https://api.anthropic.com/v1/messages`).
    pub url: String,
    /// Header names and values, in order, the API key among them. No name appears twice.
    pub headers: Vec<(String, String)>,
    /// The JSON body; the transport serializes it.
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
    /// The HTTP status code, whatever it is: the provider decides what a non-2xx means.
    pub status: u16,
    /// Header names and values as received. The provider reads `retry-after`,
    /// `retry-after-ms` and `x-should-retry` from a refusal.
    pub headers: Vec<(String, String)>,
    /// The body: server-sent events for a 2xx, the API's error otherwise.
    pub body: ByteStream,
}

impl HttpResponse {
    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// Whether the status is 2xx.
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
///
/// Its message becomes the assistant message's `error_message` once the
/// provider gives up. Build one with [`TransportError::connect`] (retried) or
/// [`TransportError::other`] (never retried).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    /// What went wrong, as the human will read it.
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

/// How every model request travels: one POST, the answer as a byte stream.
///
/// A provider calls [`post`](Self::post) once per attempt; retries, SSE
/// parsing and error messages stay with the provider. An implementation:
///
/// - returns `Ok` as soon as the status and headers are in, for **any**
///   status. A provider reads a non-2xx body for the API's own error message
///   and decides from the status and headers whether to retry ([`RetryPolicy`]).
/// - returns [`TransportError::connect`] when no answer came and another try
///   may help (refused, reset, timed out), [`TransportError::other`] for
///   anything else. Only the first kind is retried.
/// - ends the body with an `Err` chunk if it breaks off. That ends the
///   message with an error; a streamed answer is never retried.
/// - gives up promptly when `cancel` fires. The provider also stops waiting
///   and drops the future, so a transport that ignores it only holds a
///   connection open longer.
///
/// The trait is declared with the `async-trait` crate: implement it under
/// `#[async_trait::async_trait]`. [`ReqwestTransport`](crate::ReqwestTransport)
/// is the default; the crate docs have a transport that answers from memory.
///
/// # Examples
///
/// A transport that logs each call and hands it to another:
///
/// ```
/// use std::sync::Arc;
///
/// use async_trait::async_trait;
/// use haest_providers::{HttpRequest, HttpResponse, ReqwestTransport, Transport, TransportError};
/// use tokio_util::sync::CancellationToken;
///
/// struct Logged<T>(T);
///
/// #[async_trait]
/// impl<T: Transport> Transport for Logged<T> {
///     async fn post(
///         &self,
///         req: HttpRequest,
///         cancel: CancellationToken,
///     ) -> Result<HttpResponse, TransportError> {
///         eprintln!("POST {}", req.url);
///         let resp = self.0.post(req, cancel).await;
///         if let Ok(r) = &resp {
///             eprintln!("  -> {}", r.status);
///         }
///         resp
///     }
/// }
///
/// let transport: Arc<dyn Transport> = Arc::new(Logged(ReqwestTransport::new()));
/// ```
///
/// [`RetryPolicy`]: crate::RetryPolicy
#[async_trait]
pub trait Transport: Send + Sync {
    /// POST `req`. Returns once the status and headers are in; the body streams.
    /// Should give up promptly when `cancel` fires (the caller also stops waiting).
    ///
    /// # Errors
    ///
    /// A [`TransportError`] when no answer arrived: retryable if a new
    /// connection may help. A non-2xx status is not an error here.
    async fn post(
        &self,
        req: HttpRequest,
        cancel: CancellationToken,
    ) -> Result<HttpResponse, TransportError>;
}
