//! What came back: the status and headers at once, the body as it arrives.

use std::{
    fmt,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{Stream, StreamExt, stream::BoxStream};
use serde_json::Value;

use crate::{
    HttpError,
    redact::HeaderNames,
    request::{find_header, set_header},
};

/// A response body, chunk by chunk as it arrives. Chunks may split a line, an
/// event or a UTF-8 character anywhere; an `Err` chunk ends the body.
///
/// It is a [`Stream`] of chunks (read it with `futures::StreamExt::next`), or
/// read it whole with [`bytes`](Self::bytes).
///
/// ```
/// use haest_http::{Body, Bytes, HttpError};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), HttpError> {
/// let whole = Body::from("hello").bytes(1024).await?;
/// assert_eq!(whole, Bytes::from("hello"));
///
/// // A body past the reader's limit is refused, never cut.
/// let big = Body::from(vec![0u8; 2048]).bytes(1024).await;
/// assert!(matches!(big, Err(HttpError::TooLarge(1024))));
///
/// // A client that streams hands over its chunks as they come.
/// let chunks = futures::stream::iter([Ok(Bytes::from("hel")), Ok(Bytes::from("lo"))]);
/// assert_eq!(Body::from_stream(chunks).bytes(1024).await?, Bytes::from("hello"));
/// # Ok(())
/// # }
/// ```
pub struct Body(BoxStream<'static, Result<Bytes, HttpError>>);

impl Body {
    /// No body.
    pub fn empty() -> Self {
        Self(futures::stream::empty().boxed())
    }

    /// The body as a stream of chunks, as a client reads them.
    pub fn from_stream(
        chunks: impl Stream<Item = Result<Bytes, HttpError>> + Send + 'static,
    ) -> Self {
        Self(chunks.boxed())
    }

    /// The whole body, at most `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`HttpError::TooLarge`] when it passes `limit` (nothing past it is
    /// read), or the error a chunk ended the body with.
    pub async fn bytes(mut self, limit: usize) -> Result<Bytes, HttpError> {
        let mut whole = Vec::new();
        while let Some(chunk) = self.0.next().await {
            let chunk = chunk?;
            if whole.len() + chunk.len() > limit {
                return Err(HttpError::TooLarge(limit));
            }
            whole.extend_from_slice(&chunk);
        }
        Ok(whole.into())
    }
}

impl Stream for Body {
    type Item = Result<Bytes, HttpError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_next_unpin(cx)
    }
}

impl From<Bytes> for Body {
    fn from(whole: Bytes) -> Self {
        Self(futures::stream::iter([Ok(whole)]).boxed())
    }
}

impl From<Vec<u8>> for Body {
    fn from(whole: Vec<u8>) -> Self {
        Bytes::from(whole).into()
    }
}

impl From<String> for Body {
    fn from(whole: String) -> Self {
        Bytes::from(whole).into()
    }
}

impl From<&'static str> for Body {
    fn from(whole: &'static str) -> Self {
        Bytes::from_static(whole.as_bytes()).into()
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Body { .. }")
    }
}

/// What came back: the status and headers as soon as they arrive, the body
/// as it streams.
///
/// An error status is a response like any other: the caller reads the status.
///
/// ```
/// use haest_http::Response;
/// use serde_json::json;
///
/// let res = Response::new(404, "")
///     .with_json(&json!({"message": "Not Found"}))
///     .with_header("x-request-id", "7");
/// assert!(!res.is_success());
/// assert_eq!(res.header("X-Request-Id"), Some("7"));
/// ```
///
/// Its Debug output shows each header's name without its value (a
/// `set-cookie` is a credential).
pub struct Response {
    /// The status code, whatever it is.
    pub status: u16,
    /// Header names and values as received; read one with [`header`](Self::header).
    pub headers: Vec<(String, String)>,
    /// The body, as it arrives.
    pub body: Body,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &HeaderNames(&self.headers))
            .field("body", &self.body)
            .finish()
    }
}

impl Response {
    /// `status` with `body`, no headers (a fake's answer).
    pub fn new(status: u16, body: impl Into<Body>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// This response with `body` as its JSON text, and
    /// `content-type: application/json`.
    pub fn with_json(mut self, body: &Value) -> Self {
        self.body = body.to_string().into();
        self.with_header("content-type", "application/json")
    }

    /// This response with `name: value`. A header of the same name, in any
    /// case, is replaced, as on a [`Request`](crate::Request).
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        set_header(&mut self.headers, name.into(), value.into());
        self
    }

    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// Whether the status is a success (2xx).
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}
