//! The default [`HttpClient`]: reqwest over rustls, and the one place haest
//! builds a reqwest client.

use std::{
    fmt,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{StatusCode, redirect};

use crate::{Body, HttpClient, HttpError, Method, Request, Response};

/// How many redirects one request may follow.
const MAX_REDIRECTS: usize = 10;

/// reqwest's client builder as haest starts every client. Change either on
/// the builder when you want otherwise; build it with [`build_reqwest`].
///
/// - **No proxy**, whatever `HTTP(S)_PROXY`, `ALL_PROXY` or the OS's settings
///   say. The environment does not shape a run, and a proxy haest was not
///   handed would be sent the keys and tokens (in clear, for an `http://` URL).
/// - **A redirect is followed only within the request's origin** (scheme,
///   host and port) **and only a 307 or 308**, which repeat the request as it
///   was sent; at most 10. Any other redirect comes back as the answer. On a
///   redirect to another origin reqwest drops only `Authorization`, `Cookie`
///   and `Proxy-Authorization`: an `x-api-key`, a host's own headers and a
///   307's body (a whole prompt) would go along. A 301, 302 or 303 turns a
///   POST into a GET without its body, a different request than the one sent.
pub fn reqwest_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(same_origin_redirects())
}

/// Follow a 307 or 308 to the first request's origin; hand back any other redirect.
fn same_origin_redirects() -> redirect::Policy {
    redirect::Policy::custom(|attempt| {
        let keeps_request = matches!(
            attempt.status(),
            StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
        );
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|first| first.origin() == attempt.url().origin());
        if !keeps_request || !same_origin {
            attempt.stop()
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error(format!("more than {MAX_REDIRECTS} redirects"))
        } else {
            attempt.follow()
        }
    })
}

/// Build `builder` into a client: the one place haest builds one.
///
/// reqwest is built here with rustls and no crypto provider of its own
/// (`rustls-no-provider`, so the binary carries one crypto stack, ring, and
/// needs no C toolchain). Building such a client in a process with no rustls
/// provider installed panics inside reqwest. This installs ring as the
/// process's provider first when none is installed, so building cannot panic
/// for want of one; a provider the host installed is kept.
///
/// # Errors
///
/// [`HttpError::Request`] when reqwest refuses the builder's settings (a bad
/// certificate or proxy, say).
pub fn build_reqwest(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, HttpError> {
    // Err = a provider is installed already (the host's, or ours): it is kept.
    let _ = rustls::crypto::ring::default_provider().install_default();
    builder.build().map_err(|e| HttpError::Request(e.into()))
}

/// Settings on haest's builder: what [`ReqwestClient::configured`] takes.
type Configure = dyn Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder + Send + Sync;

/// A client of its own: its settings, and the client once built.
struct Own {
    configure: Box<Configure>,
    built: OnceLock<reqwest::Client>,
}

/// The default [`HttpClient`]: reqwest over rustls (ring), HTTP/1.1 and 2,
/// no proxy, a redirect followed only within the request's origin
/// ([`reqwest_builder`]), no timeout of its own. The body streams as it
/// arrives.
///
/// Building one never builds a reqwest client and never fails: the client is
/// built on the first request, and a failure to build is that request's
/// [`HttpError::Request`]. [`new`](Self::new) (also `Default`) is the
/// process's one shared client: every `ReqwestClient::new()` in the process
/// sends through one connection pool. [`configured`](Self::configured) is a
/// client of its own, with the host's settings. Cloning is cheap: clones
/// share the client.
///
/// ```
/// use std::time::Duration;
///
/// use haest_http::{HttpClientRef, ReqwestClient, reqwest};
///
/// # fn main() -> Result<(), haest_http::BoxError> {
/// // What the model calls, GenerateImage and the GitHub tracker use unless handed another.
/// let shared: HttpClientRef = ReqwestClient::new().into();
///
/// // A host's own: through its proxy, a shorter connect timeout.
/// let proxy = reqwest::Proxy::all("http://proxy.internal:3128")?;
/// let own = ReqwestClient::configured(move |builder| {
///     builder.proxy(proxy.clone()).connect_timeout(Duration::from_secs(5))
/// });
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default)]
pub struct ReqwestClient {
    /// None = the process's shared client.
    own: Option<Arc<Own>>,
}

impl ReqwestClient {
    /// The process's shared client, built on the first request through any
    /// `ReqwestClient::new()`.
    ///
    /// That first request also installs ring as the process's rustls
    /// provider when none is installed ([`build_reqwest`]), for the whole
    /// process: a host that wants its own provider installs it before then.
    pub fn new() -> Self {
        Self::default()
    }

    /// A client of its own: [`reqwest_builder`] with `configure`'s settings
    /// on top (a proxy, root certificates, timeouts, a redirect policy, a DNS
    /// resolver), built on its first request and kept. If it does not build,
    /// each request says why and the next one tries again.
    pub fn configured(
        configure: impl Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder + Send + Sync + 'static,
    ) -> Self {
        Self {
            own: Some(Arc::new(Own {
                configure: Box::new(configure),
                built: OnceLock::new(),
            })),
        }
    }

    /// The reqwest client, built now if this is the first request.
    fn client(&self) -> Result<reqwest::Client, HttpError> {
        static SHARED: OnceLock<reqwest::Client> = OnceLock::new();
        let (cell, builder) = match &self.own {
            None => (&SHARED, None),
            Some(own) => (&own.built, Some(&own.configure)),
        };
        if let Some(client) = cell.get() {
            return Ok(client.clone());
        }
        let configured = match builder {
            None => reqwest_builder(),
            Some(configure) => configure(reqwest_builder()),
        };
        let built = build_reqwest(configured)?;
        // Two first requests at once may both build; one client is kept.
        Ok(cell.get_or_init(|| built).clone())
    }
}

impl fmt::Debug for ReqwestClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let which = if self.own.is_some() { "own" } else { "shared" };
        f.debug_tuple("ReqwestClient").field(&which).finish()
    }
}

/// [`ReqwestClient::send`].
#[async_trait]
impl HttpClient for ReqwestClient {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        ReqwestClient::send(self, req).await
    }
}

impl ReqwestClient {
    /// Sends `req` and returns the answer once its status and headers are
    /// in; the body streams after (the [`HttpClient`] method, callable
    /// without importing the trait).
    ///
    /// # Errors
    ///
    /// [`HttpError::Request`] when the client does not build or reqwest
    /// refuses the request (a redirect off the origin among them), and
    /// [`HttpError::Connect`] when the server cannot be reached or does not
    /// answer.
    pub async fn send(&self, req: Request) -> Result<Response, HttpError> {
        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client()?.request(method, &req.url);
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        if !req.body.is_empty() {
            builder = builder.body(req.body);
        }
        let res = builder.send().await.map_err(|e| {
            if e.is_builder() || e.is_redirect() {
                HttpError::Request(e.into())
            } else {
                HttpError::Connect(e.into())
            }
        })?;
        let headers = res
            .headers()
            .iter()
            .map(|(k, v)| {
                let value = String::from_utf8_lossy(v.as_bytes()).into_owned();
                (k.as_str().to_string(), value)
            })
            .collect();
        Ok(Response {
            status: res.status().as_u16(),
            headers,
            body: Body::from_stream(
                res.bytes_stream()
                    .map(|chunk| chunk.map_err(|e| HttpError::Body(e.into()))),
            ),
        })
    }
}
