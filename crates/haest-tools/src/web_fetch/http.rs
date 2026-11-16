//! WebFetch's HTTP seam: one GET, never following a redirect (WebFetch follows
//! them by hand so the net guard sees every hop), connecting only to the
//! addresses the guard checked.
//!
//! PLUG POINT: implement [`HttpGet`] for a host's own transport (or a test's fake
//! web). [`ReqwestGet`] is the default.

use std::net::{IpAddr, SocketAddr};

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use url::{Host, Url};

/// A response body, chunk by chunk, so a cap can stop it without buffering it all.
pub type Body = BoxStream<'static, Result<Vec<u8>, String>>;

pub struct HttpResponse {
    pub status: u16,
    /// Header names in any case; read them with [`HttpResponse::header`].
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl HttpResponse {
    /// A whole body in one chunk (tests, a host whose transport buffers).
    pub fn buffered(status: u16, headers: &[(&str, &str)], body: impl Into<Vec<u8>>) -> Self {
        let body: Vec<u8> = body.into();
        Self {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: futures::stream::iter([Ok(body)]).boxed(),
        }
    }

    /// One header's value; the name is matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[async_trait]
pub trait HttpGet: Send + Sync {
    /// ONE GET of `url`; a redirect comes back as a 3xx response, never followed.
    /// `pinned` are the addresses the net guard checked for the URL's host: an
    /// implementation that resolves names itself connects only to those.
    async fn get(
        &self,
        url: &Url,
        pinned: &[IpAddr],
        user_agent: &str,
    ) -> Result<HttpResponse, String>;
}

/// The default: reqwest with rustls, no proxy, no redirects, DNS pinned to `pinned`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReqwestGet;

#[async_trait]
impl HttpGet for ReqwestGet {
    async fn get(
        &self,
        url: &Url,
        pinned: &[IpAddr],
        user_agent: &str,
    ) -> Result<HttpResponse, String> {
        // reqwest takes the process-wide provider when one is installed; this
        // installs ring only when nobody has installed one yet.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent(user_agent);
        if let Some(Host::Domain(name)) = url.host() {
            // The port in a pinned address is ignored: the URL's port is used.
            let addrs: Vec<SocketAddr> = pinned.iter().map(|ip| SocketAddr::new(*ip, 0)).collect();
            builder = builder.resolve_to_addrs(name, &addrs);
        }
        let client = builder.build().map_err(|e| e.to_string())?;
        let res = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| format!("could not fetch {url}: {e}"))?;
        let headers = res
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let status = res.status().as_u16();
        // Chunks until the end or the first error; the response drops with the stream.
        let body = futures::stream::unfold(Some(res), |state| async move {
            let mut res = state?;
            match res.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk.to_vec()), Some(res))),
                Ok(None) => None,
                Err(e) => Some((Err(e.to_string()), None)),
            }
        })
        .boxed();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
