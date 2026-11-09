//! The default [`Transport`]: reqwest over rustls, the body streamed as it arrives.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpRequest, HttpResponse, Transport, TransportError};

/// How long to wait for the status line. The official SDKs wait ten minutes:
/// a long prompt can sit in a queue before the first byte.
const HEADERS_TIMEOUT: Duration = Duration::from_secs(600);

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
    pub fn new() -> Self {
        Self::with_client(reqwest::Client::new())
    }

    /// Bring a configured client (proxy, root certificates, connection pool).
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
