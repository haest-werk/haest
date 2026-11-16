//! The GitHub backend's HTTP seam: one request in, the status and the whole
//! body out.
//!
//! haest-providers' `Transport` only POSTs (a model call is one JSON POST read
//! as a stream), and haest-tools' `HttpGet` / `ImageHttp` only GET and POST;
//! the REST API also needs PATCH and DELETE. So this is one more narrow seam of
//! the same shape, and [`ReqwestHttp`] is the same client those use: reqwest
//! on rustls with ring, no proxy.
//!
//! PLUG POINT: implement [`Http`] to send GitHub's requests through a host's
//! own client (a proxy it chose, a recorder), or a test's fake.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// One request. `body` goes out as JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpCall {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
}

impl HttpCall {
    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The answer: its status and whole body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HttpReply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpReply {
    pub fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            body: body.to_string().into_bytes(),
        }
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[async_trait]
pub trait Http: Send + Sync {
    /// Send `call`; Err is why no answer came (the connection, a timeout).
    async fn send(&self, call: HttpCall) -> Result<HttpReply, String>;
}

/// The most of an answer that is read; a list of 100 issues is well under it.
const MAX_BODY: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

/// The default: reqwest with rustls, no proxy (none from the environment
/// either: it would be sent the token), a 60 s limit per request.
#[derive(Debug, Clone)]
pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl Default for ReqwestHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestHttp {
    pub fn new() -> Self {
        // reqwest takes the process-wide provider when one is installed; this
        // installs ring only when nobody has installed one yet.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(TIMEOUT)
            .build()
            .expect("an HTTP client with the ring TLS provider builds");
        Self::with_client(client)
    }

    /// Bring a configured client (a proxy, root certificates).
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Http for ReqwestHttp {
    async fn send(&self, call: HttpCall) -> Result<HttpReply, String> {
        let method = match call.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut req = self.client.request(method, &call.url);
        for (name, value) in &call.headers {
            req = req.header(name, value);
        }
        if let Some(body) = &call.body {
            req = req.body(body.to_string());
        }
        let mut res = req.send().await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(format!("the answer passed {MAX_BODY} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpReply { status, body })
    }
}
