//! Why a request got no answer, or its answer broke off.

use std::{error::Error, time::Duration};

/// Any error, as a cause: `"text".into()` and `e.into()` both make one.
pub type BoxError = Box<dyn Error + Send + Sync>;

/// Why a request got no answer, or its answer broke off.
///
/// An answer with an error status (a 404, a 529) is not an `HttpError`: it is
/// a [`Response`](crate::Response) whose status says so. A variant's own text
/// is short ("no connection"); its cause is its [`source`](Error::source),
/// never in that text. Show one with its causes (haest-core's
/// `error::report`).
///
/// ```
/// use std::error::Error;
///
/// use haest_http::HttpError;
///
/// let refused = HttpError::Connect("connection refused".into());
/// assert!(refused.is_retryable());
/// // The cause is the source, not part of the variant's own words.
/// assert_eq!(refused.source().unwrap().to_string(), "connection refused");
///
/// assert!(!HttpError::TooLarge(1024).is_retryable());
/// ```
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// No answer came: the name did not resolve, the connection was refused
    /// or reset, TLS failed, a timeout set on the client ran out. Another try
    /// may help.
    #[error("no connection")]
    Connect(#[source] BoxError),
    /// No status within the time the caller allowed. Another try may help.
    #[error("no answer within {} s", .0.as_secs())]
    Timeout(Duration),
    /// The answer started, then its body broke off.
    #[error("the answer broke off")]
    Body(#[source] BoxError),
    /// The body is longer than the reader allows, in bytes.
    #[error("the answer passed {0} bytes")]
    TooLarge(usize),
    /// The request cannot be sent, or finished, as given: a URL or a header
    /// HTTP cannot carry, a redirect loop, a client that does not build.
    /// Another try fails the same way.
    #[error("the request cannot be sent")]
    Request(#[source] BoxError),
}

impl HttpError {
    /// Whether another try may get an answer: [`Connect`](Self::Connect) and
    /// [`Timeout`](Self::Timeout).
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Timeout(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_answer_is_worth_another_try() {
        assert!(HttpError::Timeout(Duration::from_secs(1)).is_retryable());
        assert!(!HttpError::Body("cut".into()).is_retryable());
        assert!(!HttpError::Request("bad header".into()).is_retryable());
    }
}
