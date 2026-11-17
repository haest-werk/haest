//! [`HttpClientRef`]: the handle an [`HttpClient`] is held by.

use std::{fmt, ops::Deref, sync::Arc};

use crate::HttpClient;
#[cfg(not(feature = "reqwest"))]
use crate::{HttpError, Request, Response};

/// An [`HttpClient`], shared: what a provider, a tracker or a tool holds.
/// Built from the client itself, or from an `Arc` of one the host already
/// shares, so no signature names `Arc` or `dyn`.
///
/// A parameter that takes `impl Into<HttpClientRef>` takes the client itself
/// (`ReqwestClient::new()`, a host's own), an `Arc` of one, an
/// `Arc<dyn HttpClient>`, a `Box<dyn HttpClient>` (a client picked at run
/// time), or another handle. Cloning is cheap: the clones share one client.
/// It derefs to the client, so `http.send(req)` works on the handle.
///
/// [`Default`] is what every part that sends uses unless handed another:
/// the process's shared `ReqwestClient` with the `reqwest` feature (the
/// default), else a client that refuses every request with
/// [`HttpError::Request`](crate::HttpError::Request).
///
/// ```
/// use std::sync::Arc;
///
/// use haest_http::{HttpClient, HttpClientRef, HttpError, Request, Response, async_trait};
///
/// struct Teapot;
///
/// #[async_trait]
/// impl HttpClient for Teapot {
///     async fn send(&self, _req: Request) -> Result<Response, HttpError> {
///         Ok(Response::new(418, ""))
///     }
/// }
///
/// // The client itself; a clone of the handle shares it.
/// let http: HttpClientRef = Teapot.into();
/// let again = http.clone();
/// assert!(HttpClientRef::ptr_eq(&http, &again));
///
/// // A host that already holds its client in an `Arc` hands that over as it is.
/// fn handle(shared: Arc<Teapot>) -> HttpClientRef {
///     shared.into()
/// }
/// ```
#[derive(Clone)]
pub struct HttpClientRef(Arc<dyn HttpClient>);

impl HttpClientRef {
    /// A handle to `client`. [`From`] does the same, so a parameter that
    /// takes `impl Into<HttpClientRef>` takes the client itself.
    pub fn new(client: impl HttpClient + 'static) -> Self {
        Self(Arc::new(client))
    }

    /// Whether both handles hold the same client (one is a clone of the other).
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        Arc::ptr_eq(&this.0, &other.0)
    }
}

/// What every part that sends uses unless handed another client.
impl Default for HttpClientRef {
    /// The process's shared [`ReqwestClient`](crate::ReqwestClient), built on
    /// the first request. Without the `reqwest` feature there is none: a
    /// client that refuses every request with
    /// [`HttpError::Request`](crate::HttpError::Request), saying a host must
    /// hand one in.
    fn default() -> Self {
        #[cfg(feature = "reqwest")]
        let client = crate::ReqwestClient::new();
        #[cfg(not(feature = "reqwest"))]
        let client = NoClient;
        Self::new(client)
    }
}

/// The default client without the `reqwest` feature: no client at all.
#[cfg(not(feature = "reqwest"))]
#[derive(Debug, Clone, Copy)]
struct NoClient;

#[cfg(not(feature = "reqwest"))]
#[async_trait::async_trait]
impl HttpClient for NoClient {
    async fn send(&self, _req: Request) -> Result<Response, HttpError> {
        Err(HttpError::Request(NO_CLIENT.into()))
    }
}

/// What a request through the default client says without the `reqwest` feature.
#[cfg(not(feature = "reqwest"))]
const NO_CLIENT: &str =
    "no HTTP client: haest-http is built without its `reqwest` feature, so hand one in";

impl<T: HttpClient + 'static> From<T> for HttpClientRef {
    fn from(client: T) -> Self {
        Self(Arc::new(client))
    }
}

impl<T: HttpClient + 'static> From<Arc<T>> for HttpClientRef {
    fn from(client: Arc<T>) -> Self {
        Self(client)
    }
}

impl From<Arc<dyn HttpClient>> for HttpClientRef {
    fn from(client: Arc<dyn HttpClient>) -> Self {
        Self(client)
    }
}

/// A client the host picked at run time (reqwest, or its gateway's).
impl From<Box<dyn HttpClient>> for HttpClientRef {
    fn from(client: Box<dyn HttpClient>) -> Self {
        Self(client.into())
    }
}

impl AsRef<dyn HttpClient> for HttpClientRef {
    fn as_ref(&self) -> &(dyn HttpClient + 'static) {
        &*self.0
    }
}

impl Deref for HttpClientRef {
    type Target = dyn HttpClient;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl fmt::Debug for HttpClientRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HttpClientRef").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpError, Request, Response};

    struct Teapot;

    #[async_trait::async_trait]
    impl HttpClient for Teapot {
        async fn send(&self, _req: Request) -> Result<Response, HttpError> {
            Ok(Response::new(418, ""))
        }
    }

    #[tokio::test]
    async fn the_handle_sends_through_the_client_it_holds() {
        let http = HttpClientRef::new(Teapot);
        let res = http.send(Request::get("https://a.test/")).await.unwrap();
        assert_eq!(res.status, 418);
    }

    /// Without reqwest there is no client to fall back on: the default says
    /// so on the first request, and never panics or sends.
    #[cfg(not(feature = "reqwest"))]
    #[tokio::test]
    async fn without_reqwest_the_default_refuses_and_says_to_hand_one_in() {
        let err = HttpClientRef::default()
            .send(Request::get("https://a.test/"))
            .await
            .unwrap_err();
        assert!(!err.is_retryable());
        let cause = std::error::Error::source(&err).unwrap().to_string();
        assert!(cause.contains("hand one in"), "{cause}");
    }

    #[test]
    fn a_shared_client_stays_one_client_in_every_slot() {
        let shared: Arc<dyn HttpClient> = Arc::new(Teapot);
        let a = HttpClientRef::from(shared.clone());
        let b = HttpClientRef::from(shared);
        assert!(HttpClientRef::ptr_eq(&a, &b));
        assert!(HttpClientRef::ptr_eq(&a, &a.clone()));
        assert_eq!(format!("{a:?}"), "HttpClientRef(..)");
    }

    /// A client the host picked at run time goes in boxed, as on every
    /// other handle.
    #[tokio::test]
    async fn a_boxed_client_goes_in_as_it_is() {
        let picked: Box<dyn HttpClient> = Box::new(Teapot);
        let http: HttpClientRef = picked.into();
        let res = http.send(Request::get("https://a.test/")).await.unwrap();
        assert_eq!(res.status, 418);
    }
}
