//! The default [`Transport`]: reqwest over rustls, the body streamed as it arrives.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpRequest, HttpResponse, Transport, TransportError};

/// How long to wait for the status line. The official SDKs wait ten minutes:
/// a long prompt can sit in a queue before the first byte.
const HEADERS_TIMEOUT: Duration = Duration::from_secs(600);

/// The default [`Transport`]: HTTP over reqwest and rustls, straight to the
/// server, the body streamed as it arrives.
///
/// It reads no proxy settings and nothing else from the environment. It waits
/// up to ten minutes for the status and headers (a long prompt can queue before
/// the first byte; the official SDKs allow as long), then the body takes as
/// long as it takes. A connection failure or that timeout is a retryable
/// [`TransportError`](crate::TransportError); a cancelled request is not.
/// Cloning is cheap: clones share one connection pool.
///
/// ```
/// use std::{sync::Arc, time::Duration};
///
/// use haest_providers::{ReqwestTransport, Transport};
///
/// let transport: Arc<dyn Transport> =
///     Arc::new(ReqwestTransport::new().headers_timeout(Duration::from_secs(120)));
/// ```
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
    headers_timeout: Duration,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestTransport {
    /// Straight to the server: no proxy, whatever `HTTP(S)_PROXY`, `ALL_PROXY`
    /// or the OS's network settings say. The environment does not shape a run
    /// (and a key for an `http://` base URL would reach the proxy in clear); a
    /// host that wants a proxy hands in its client ([`Self::with_client`]).
    ///
    /// Installs ring as the process's rustls crypto provider when none is
    /// installed yet; one the host installed first is kept.
    ///
    /// # Panics
    ///
    /// If reqwest cannot build its client. With a crypto provider installed
    /// the line before, that does not happen.
    pub fn new() -> Self {
        // reqwest takes the process-wide TLS provider; this installs ring only
        // when the host has not installed one of its own.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("an HTTP client with the ring TLS provider builds");
        Self::with_client(client)
    }

    /// Bring a configured client (proxy, root certificates, connection pool).
    ///
    /// The client is used as it is: a proxy or a timeout set on it applies.
    /// This crate builds reqwest with rustls but no crypto provider of its own
    /// (`rustls-no-provider`). Unless another dependency gives reqwest one,
    /// building a client before a provider is installed panics: install one
    /// first, or call [`Self::new`] once, which installs ring.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use haest_providers::ReqwestTransport;
    ///
    /// let _ = rustls::crypto::ring::default_provider().install_default();
    /// let client = reqwest::Client::builder()
    ///     .connect_timeout(Duration::from_secs(10))
    ///     .build()?;
    /// let transport = ReqwestTransport::with_client(client);
    /// # Ok::<(), reqwest::Error>(())
    /// ```
    pub fn with_client(client: reqwest::Client) -> Self {
        Self {
            client,
            headers_timeout: HEADERS_TIMEOUT,
        }
    }

    /// Time allowed until the status and headers arrive; the body may take longer.
    pub fn headers_timeout(mut self, timeout: Duration) -> Self {
        self.headers_timeout = timeout;
        self
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn post(
        &self,
        req: HttpRequest,
        cancel: CancellationToken,
    ) -> Result<HttpResponse, TransportError> {
        let body =
            serde_json::to_vec(&req.body).map_err(|e| TransportError::other(e.to_string()))?;
        let mut builder = self.client.post(&req.url).body(body);
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        let sent = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(TransportError::other("request aborted")),
            sent = tokio::time::timeout(self.headers_timeout, builder.send()) => sent,
        };
        let resp = match sent {
            Err(_) => return Err(TransportError::connect("request timed out")),
            Ok(Err(e)) if e.is_builder() => return Err(TransportError::other(e.to_string())),
            Ok(Err(e)) => return Err(TransportError::connect(connection_message(&e))),
            Ok(Ok(resp)) => resp,
        };
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        Ok(HttpResponse {
            status: resp.status().as_u16(),
            headers,
            body: resp
                .bytes_stream()
                .map(|chunk| chunk.map_err(|e| TransportError::other(e.to_string())))
                .boxed(),
        })
    }
}

/// reqwest's top-level message is generic ("error sending request"); the cause says what broke.
fn connection_message(e: &reqwest::Error) -> String {
    let mut message = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
