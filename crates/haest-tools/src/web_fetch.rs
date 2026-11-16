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
//! a guarded reqwest client unless a host (or a test) injects its own. Start
//! from [`WebFetchTool`].

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use haest_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str};
use haest_http::{HttpClient, HttpError, Request, Response};
use serde_json::{Value, json};
use url::Url;

use crate::{
    canonical::pick,
    net_guard::{Resolve, SystemResolve, assert_public_url, host_port},
    read::media_output,
};

mod http;

const MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "Mozilla/5.0 (compatible; haest-webfetch/1)";
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

/// `html` as markdown: ATX headings, fenced code, no scripts, styles or embeds
/// (`script`, `style`, `noscript`, `iframe`, `svg`, `template`). HTML the
/// converter cannot read comes back as it is.
///
/// ```
/// use haest_tools::web_fetch::markdown_of;
///
/// let md = markdown_of("<h1>Title</h1><script>track()</script><p>Body</p>");
/// assert!(md.starts_with("# Title") && md.contains("Body") && !md.contains("track"));
/// ```
pub fn markdown_of(html: &str) -> String {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(SKIPPED_TAGS.to_vec())
        .build()
        .convert(html)
        .unwrap_or_else(|_| html.to_string())
}

/// WebFetch's two network seams: how names resolve and how a GET is made.
///
/// [`new`](Self::new) (and `Default`, over the system resolver,
/// [`SystemResolve`]) pairs the resolver with WebFetch's own reqwest client,
/// which follows no redirect and, when it connects, resolves through the same
/// resolver and refuses any blocked address. A host with its own DNS starts
/// from `new(its_resolver)`; a host with its own HTTP stack, or a test with a
/// fake web, replaces `http`. That client is sent one GET per hop and must not
/// follow redirects; it should connect only to public addresses.
#[derive(Clone)]
pub struct WebFetchNet {
    /// Name -> addresses, for the net guard.
    pub resolve: Arc<dyn Resolve>,
    /// One GET per hop, redirects handed back, never followed.
    pub http: Arc<dyn HttpClient>,
}

impl WebFetchNet {
    /// The guard over `resolve`, and WebFetch's own client resolving through
    /// it. Nothing is built until the first fetch.
    pub fn new(resolve: Arc<dyn Resolve>) -> Self {
        Self {
            http: Arc::new(http::guarded_client(resolve.clone())),
            resolve,
        }
    }
}

impl Default for WebFetchNet {
    fn default() -> Self {
        Self::new(Arc::new(SystemResolve))
    }
}

/// GET `url`, following redirects by hand so each hop is guarded.
async fn guarded_get(url: &Url, net: &WebFetchNet) -> Result<(Response, Url), ToolError> {
    let mut at = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        assert_public_url(&at, net.resolve.as_ref())
            .await
            .map_err(ToolError::Input)?;
        let get = Request::get(at.as_str()).with_header("user-agent", USER_AGENT);
        let res = net
            .http
            .send(get)
            .await
            .map_err(|e| ToolError::Input(format!("could not fetch {at}: {}", e.report())))?;
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
async fn capped_body(res: Response) -> Result<Vec<u8>, ToolError> {
    let declared: u64 = res
        .header("content-length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if declared > MAX_BYTES as u64 {
        return Err(ToolError::Input(format!(
            "the response is {declared} bytes; the limit is {MAX_BYTES}"
        )));
    }
    match res.body.bytes(MAX_BYTES).await {
        Ok(body) => Ok(body.to_vec()),
        Err(HttpError::TooLarge(_)) => Err(ToolError::Input(format!(
            "the response passed the {MAX_BYTES}-byte limit"
        ))),
        Err(e) => Err(ToolError::Input(e.report())),
    }
}

async fn to_output(url: &Url, res: Response, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let media_type = res
        .header("content-type")
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let (status, ok) = (res.status, res.is_success());
    let body = capped_body(res).await?;
    let head = format!("{url} -- HTTP {status} {media_type}, {} bytes", body.len());
    if is_text_type(&media_type) || !ok {
        let text = String::from_utf8_lossy(&body);
        let shown = if media_type.contains("html") {
            markdown_of(&text)
        } else {
            text.into_owned()
        };
        return Ok(ToolOutput {
            content: format!("{head}\n\n{shown}"),
            is_error: !ok,
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

/// The WebFetch tool (`WebFetch`, kind `web.fetch`, read-only): one public
/// web page into the context.
///
/// Input: `url`. One GET with redirects followed by hand (at most 5, each hop
/// checked by the net guard), a 5 MB cap on the body (refused, never cut) and
/// a 30 s timeout. HTML comes back as markdown ([`markdown_of`]), other text
/// as it is, an image, PDF, audio or video as media; an HTTP error status is
/// an error result with the body. A refusal, a timeout or an interrupt is a
/// [`ToolError::Input`] the model reads.
///
/// # Examples
///
/// Over a fake web, so nothing leaves the process:
///
/// ```
/// use std::{net::IpAddr, sync::Arc};
///
/// use async_trait::async_trait;
/// use haest_core::{
///     tool::{Tool, ToolContext},
///     workspace::ShutWorkspace,
/// };
/// use haest_tools::{
///     WebFetchNet, WebFetchTool,
///     http::{HttpClient, HttpError, Request, Response},
///     net_guard::Resolve,
/// };
/// use serde_json::json;
///
/// /// Every name is one public address that serves one page.
/// struct FakeWeb;
///
/// #[async_trait]
/// impl Resolve for FakeWeb {
///     async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
///         Ok(vec!["93.184.215.14".parse().unwrap()])
///     }
/// }
///
/// #[async_trait]
/// impl HttpClient for FakeWeb {
///     async fn send(&self, _req: Request) -> Result<Response, HttpError> {
///         let html = "<h1>Hello</h1><script>track()</script>";
///         Ok(Response::new(200, html).with_header("Content-Type", "text/html"))
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let web = Arc::new(FakeWeb);
/// let tool = WebFetchTool::new(WebFetchNet { resolve: web.clone(), http: web });
/// let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
/// let input = json!({"url": "https://example.test/"});
/// let out = tool.execute(input.as_object().unwrap().clone(), &ctx).await.unwrap();
/// assert!(out.content.contains("# Hello") && !out.content.contains("track"));
///
/// // A private address is refused before any request is made.
/// let input = json!({"url": "http://127.0.0.1:8080/admin"});
/// assert!(tool.execute(input.as_object().unwrap().clone(), &ctx).await.is_err());
/// # }
/// ```
pub struct WebFetchTool {
    net: WebFetchNet,
}

impl WebFetchTool {
    /// WebFetch over `net`.
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
