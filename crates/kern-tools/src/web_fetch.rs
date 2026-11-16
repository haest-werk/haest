//! WebFetch: one public web page into the context, as markdown.
//!
//! Runs in the HOST, never through the workspace: the sandbox has no network on
//! purpose, and this tool is the one narrow door -- GET only, public addresses
//! only ([`crate::net_guard`], checked again on every redirect hop), a byte cap
//! and a timeout. HTML becomes markdown (scripts and styles dropped); other text
//! comes back as it is; an image / PDF / audio / video is attached like a Read of
//! a file. A long page goes through the generic spill to a file the model can
//! Read in pieces.
//!
//! PLUG POINT: [`WebFetchNet`] -- the resolver and the HTTP client, real DNS and
//! reqwest unless a host (or a test) injects its own.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::StreamExt;
use kern_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str};
use serde_json::{Value, json};
use url::Url;

use crate::{
    canonical::pick,
    net_guard::{Resolve, SystemResolve, assert_public_url, host_port},
    read::media_output,
};

mod http;

pub use http::{Body, HttpGet, HttpResponse, ReqwestGet};

const MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "Mozilla/5.0 (compatible; kern-webfetch/1)";
const TEXT_PREFIXES: [&str; 8] = [
    "text/",
    "application/json",
    "application/xml",
    "application/xhtml+xml",
    "application/javascript",
    "application/ld+json",
    "application/rss+xml",
    "application/atom+xml",
];
const SKIPPED_TAGS: [&str; 6] = ["script", "style", "noscript", "iframe", "svg", "template"];

/// A type whose body is text the model reads as is (or as markdown, for HTML).
fn is_text_type(media_type: &str) -> bool {
    TEXT_PREFIXES.iter().any(|p| media_type.starts_with(p)) || media_type.ends_with("+json")
}

/// HTML as markdown: ATX headings, fenced code, no scripts / styles / embeds.
pub fn markdown_of(html: &str) -> String {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(SKIPPED_TAGS.to_vec())
        .build()
        .convert(html)
        .unwrap_or_else(|_| html.to_string())
}

/// The two network seams.
#[derive(Clone)]
pub struct WebFetchNet {
    pub resolve: Arc<dyn Resolve>,
    pub http: Arc<dyn HttpGet>,
}

impl Default for WebFetchNet {
    fn default() -> Self {
        Self {
            resolve: Arc::new(SystemResolve),
            http: Arc::new(ReqwestGet),
        }
    }
}

/// GET `url`, following redirects by hand so each hop is guarded.
async fn guarded_get(url: &Url, net: &WebFetchNet) -> Result<(HttpResponse, Url), ToolError> {
    let mut at = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let pinned = assert_public_url(&at, net.resolve.as_ref())
            .await
            .map_err(ToolError::Input)?;
        let res = net
            .http
            .get(&at, &pinned, USER_AGENT)
            .await
            .map_err(ToolError::Input)?;
        let location = match res.status {
            300..=399 => res.header("location").map(str::to_string),
            _ => None,
        };
        let Some(next) = location else {
            return Ok((res, at));
        };
        drop(res);
        at = at
            .join(&next)
            .map_err(|e| ToolError::Input(format!("bad redirect from {at} to \"{next}\": {e}")))?;
    }
    Err(ToolError::Input(format!(
        "more than {MAX_REDIRECTS} redirects starting at {url}"
    )))
}

/// The body, refusing anything past MAX_BYTES instead of buffering it.
async fn capped_body(res: &mut HttpResponse) -> Result<Vec<u8>, ToolError> {
    let declared: u64 = res
        .header("content-length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if declared > MAX_BYTES as u64 {
        return Err(ToolError::Input(format!(
            "the response is {declared} bytes; the limit is {MAX_BYTES}"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.body.next().await {
        let chunk = chunk.map_err(ToolError::Input)?;
        if body.len() + chunk.len() > MAX_BYTES {
            return Err(ToolError::Input(format!(
                "the response passed the {MAX_BYTES}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn to_output(
    url: &Url,
    mut res: HttpResponse,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let media_type = res
        .header("content-type")
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let body = capped_body(&mut res).await?;
    let head = format!(
        "{url} -- HTTP {} {media_type}, {} bytes",
        res.status,
        body.len()
    );
    if is_text_type(&media_type) || !res.ok() {
        let text = String::from_utf8_lossy(&body);
        let shown = if media_type.contains("html") {
            markdown_of(&text)
        } else {
            text.into_owned()
        };
        return Ok(ToolOutput {
            content: format!("{head}\n\n{shown}"),
            is_error: !res.ok(),
            ..Default::default()
        });
    }
    let label = url
        .path_segments()
        .and_then(|mut s| s.next_back().map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| host_port(url));
    media_output(&label, body, &media_type, ctx).await
}

pub struct WebFetchTool {
    net: WebFetchNet,
}

impl WebFetchTool {
    pub fn new(net: WebFetchNet) -> Self {
        Self { net }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new(WebFetchNet::default())
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }
    fn kind(&self) -> &str {
        "web.fetch"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "Fetch one public web page (http/https GET) and return it as markdown. Other text (JSON, plain text) comes back as it is; an image or PDF is attached so you can see it. Private and local network addresses are refused. Long pages are saved to a file you can Read in pieces.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"url": {"type": "string", "description": "The full http(s) URL"}},
            "required": ["url"]
        })
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        pick(input, &[("url", "url")])
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = req_str(&input, "url")?;
        let url = Url::parse(raw)
            .map_err(|_| ToolError::Input(format!("\"{raw}\" is not a valid URL")))?;
        let fetch = async {
            let (res, at) = guarded_get(&url, &self.net).await?;
            to_output(&at, res, ctx).await
        };
        tokio::select! {
            _ = ctx.cancel.cancelled() => Err(ToolError::Input("the fetch was interrupted".into())),
            done = tokio::time::timeout(TIMEOUT, fetch) => done.unwrap_or_else(|_| {
                Err(ToolError::Input(format!("the fetch timed out after {}s", TIMEOUT.as_secs())))
            }),
        }
    }
}

#[cfg(test)]
mod tests;
