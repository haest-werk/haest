//! [`HttpClientRef`]: the handle an [`HttpClient`] is held by.

use std::{fmt, ops::Deref, sync::Arc};

use crate::HttpClient;

/// An [`HttpClient`], shared: what a provider, a tracker or a tool holds.
/// Built from the client itself, or from an `Arc` of one the host already
/// shares, so no signature names `Arc` or `dyn`.
///
/// A parameter that takes `impl Into<HttpClientRef>` takes the client itself
/// (`ReqwestClient::new()`, a host's own), an `Arc` of one, or another
/// handle. Cloning is cheap: the clones share one client. It derefs to the
/// client, so `http.send(req)` works on the handle.
///
/// ```
/// use std::sync::Arc;
///
/// use haest_http::{HttpClientRef, ReqwestClient};
///
/// // The client itself.
/// let http: HttpClientRef = ReqwestClient::new().into();
/// // One client the host shares with other slots goes in as it is.
/// let shared = Arc::new(ReqwestClient::new());
/// let a = HttpClientRef::from(shared.clone());
/// let b = HttpClientRef::from(shared);
/// assert!(HttpClientRef::ptr_eq(&a, &b));
/// assert!(!HttpClientRef::ptr_eq(&http, &a));
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

    #[test]
    fn a_shared_client_stays_one_client_in_every_slot() {
        let shared: Arc<dyn HttpClient> = Arc::new(Teapot);
        let a = HttpClientRef::from(shared.clone());
        let b = HttpClientRef::from(shared);
        assert!(HttpClientRef::ptr_eq(&a, &b));
        assert!(HttpClientRef::ptr_eq(&a, &a.clone()));
        assert_eq!(format!("{a:?}"), "HttpClientRef(..)");
    }
}
