//! The default [`HttpClient`]: reqwest over rustls, and the one place haest
//! builds a reqwest client.

use std::{
    fmt,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use futures::StreamExt;

use crate::{Body, HttpClient, HttpError, Method, Request, Response};

/// reqwest's client builder as haest starts every client: no proxy, whatever
/// `HTTP(S)_PROXY`, `ALL_PROXY` or the OS's settings say. The environment does
/// not shape a run, and a proxy haest was not handed would be sent the keys
/// and tokens (in clear, for an `http://` URL). Add a proxy on the builder
/// when you want one; build it with [`build_reqwest`].
pub fn reqwest_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
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
/// no proxy ([`reqwest_builder`]), redirects followed (at most 10), no
/// timeout of its own. The body streams as it arrives.
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
/// use std::{sync::Arc, time::Duration};
///
/// use haest_http::{HttpClient, ReqwestClient, reqwest};
///
/// // What every haest part uses unless it is handed another.
/// let shared: Arc<dyn HttpClient> = Arc::new(ReqwestClient::new());
///
/// // A host's own: through its proxy, a shorter connect timeout.
/// let own = ReqwestClient::configured(|builder| {
///     let proxy = reqwest::Proxy::all("http://proxy.internal:3128").expect("a proxy URL");
///     builder.proxy(proxy).connect_timeout(Duration::from_secs(5))
/// });
/// ```
#[derive(Clone, Default)]
pub struct ReqwestClient {
    /// None = the process's shared client.
    own: Option<Arc<Own>>,
}

impl ReqwestClient {
    /// The process's shared client, built on the first request through any
    /// `ReqwestClient::new()`.
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

#[async_trait]
impl HttpClient for ReqwestClient {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
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
