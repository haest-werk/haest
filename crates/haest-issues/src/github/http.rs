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

/// The HTTP methods GitHub's issue API needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `GET`: read an issue, a list, a search.
    Get,
    /// `POST`: create an issue, add a link, a GraphQL call.
    Post,
    /// `PATCH`: update an issue.
    Patch,
    /// `DELETE`: remove a link.
    Delete,
}

impl Method {
    /// The method as it goes on the wire: `GET`, `POST`, `PATCH`, `DELETE`.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// One request to GitHub, ready to send.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpCall {
    /// The method.
    pub method: Method,
    /// The whole URL, query included, already encoded.
    pub url: String,
    /// Every header to send, lowercase names (`authorization` carries the
    /// token, when there is one; `content-type` comes with a body).
    pub headers: Vec<(String, String)>,
    /// The body, sent as JSON text; None = no body.
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
    /// The HTTP status code.
    pub status: u16,
    /// The whole body, as received; GitHub answers JSON (or nothing).
    pub body: Vec<u8>,
}

impl HttpReply {
    /// A reply of `status` with `body` as its JSON text: what a fake answers.
    pub fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            body: body.to_string().into_bytes(),
        }
    }

    /// Whether the status is a success (2xx).
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// PLUG POINT: how [`GithubTracker`](super::GithubTracker) sends its
/// requests. [`ReqwestHttp`] is the default; the [tracker's
/// docs](super::GithubTracker#examples) have a fake.
#[async_trait]
pub trait Http: Send + Sync {
    /// Send `call` and read the whole answer.
    ///
    /// # Errors
    ///
    /// Why no answer came: the connection, a timeout, a body too large. An
    /// answer with an error status (a 404) is an `Ok` reply, not an `Err`.
    async fn send(&self, call: HttpCall) -> Result<HttpReply, String>;
}

/// The most of an answer that is read; a list of 100 issues is well under it.
const MAX_BODY: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

/// The default: reqwest with rustls, no proxy (none from the environment
/// either: it would be sent the token), a 60 s limit per request. An answer
/// over 16 MiB is an error.
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
    /// The default client. It installs rustls' ring crypto provider for the
    /// whole process when none is installed yet (one already installed is
    /// used as is).
    ///
    /// # Panics
    ///
    /// When reqwest cannot build the client, which does not happen with the
    /// ring provider on a supported platform.
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

    /// A configured client of the host's (a proxy, root certificates, its own
    /// timeout); the 16 MiB answer limit still holds.
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
