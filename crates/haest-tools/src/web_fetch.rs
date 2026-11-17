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
//! a guarded reqwest client unless a host (or a test) injects its own
//! ([`WebFetchNet::unguarded`] says what that gives up). Start from
//! [`WebFetchTool`].

use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use haest_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str};
use haest_http::{HttpClient, Response};
use serde_json::{Value, json};
use url::Url;

use crate::{
    canonical::pick,
    net_guard::{Resolve, SystemResolve, host_port},
    read::media_output,
};
use get::{capped_body, guarded_get};

mod get;
mod http;

const TIMEOUT: Duration = Duration::from_secs(30);
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
/// use haest_tools::markdown_of;
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
/// Before every request and every redirect hop the net guard checks the URL
/// ([`assert_public_url`](crate::net_guard::assert_public_url)): an IP-literal
/// URL by its address, a name by what the resolver answers.
/// [`new`](Self::new) (and `Default`, over the system resolver,
/// [`SystemResolve`]) adds WebFetch's own client, which follows no redirect
/// and checks a name again when it connects, so a name that answers public
/// to the guard and private to the client (DNS rebinding) is never reached.
/// A host with its own DNS starts from `new(its_resolver)`.
/// [`unguarded`](Self::unguarded) takes a client of the host's own (its HTTP
/// stack, a test's fake web) and gives that second check up.
#[derive(Clone)]
pub struct WebFetchNet {
    /// Name -> addresses, for the net guard.
    resolve: Arc<dyn Resolve>,
    /// One GET per hop, redirects handed back, never followed.
    http: Arc<dyn HttpClient>,
}

impl WebFetchNet {
    /// The guard over `resolve`, and WebFetch's own client, which resolves
    /// through it when it connects and refuses a name with any blocked
    /// address. Nothing is built until the first fetch.
    pub fn new(resolve: Arc<dyn Resolve>) -> Self {
        Self {
            http: Arc::new(http::guarded_client(resolve.clone())),
            resolve,
        }
    }

    /// WebFetch through a client of the host's own, without the check when it
    /// connects: a name that resolves to a public address for the guard and
    /// to a private one when `http` connects (DNS rebinding) is reached.
    ///
    /// The guard still checks every URL and every redirect hop through
    /// `resolve` before `http` is sent it. `http` is sent one GET per hop and
    /// must hand a redirect back instead of following it: a client that
    /// follows one itself fetches a public page's redirect to
    /// `169.254.169.254` unchecked. To keep the rebinding defence, it resolves
    /// names through `resolve` and refuses a blocked address when it connects,
    /// as [`new`](Self::new)'s does. A fake web in a test, which answers from
    /// memory, is the usual reason to use this.
    pub fn unguarded(resolve: Arc<dyn Resolve>, http: Arc<dyn HttpClient>) -> Self {
        Self { resolve, http }
    }
}

impl Default for WebFetchNet {
    fn default() -> Self {
        Self::new(Arc::new(SystemResolve))
    }
}

impl fmt::Debug for WebFetchNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebFetchNet").finish_non_exhaustive()
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
///     Resolve, WebFetchNet, WebFetchTool,
///     http::{BoxError, HttpClient, HttpError, Request, Response},
/// };
/// use serde_json::json;
///
/// /// Every name is one public address that serves one page.
/// struct FakeWeb;
///
/// #[async_trait]
/// impl Resolve for FakeWeb {
///     async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, BoxError> {
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
/// let tool = WebFetchTool::new(WebFetchNet::unguarded(web.clone(), web));
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
#[derive(Debug, Clone)]
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
            _ = ctx.cancel().cancelled() => Err(ToolError::Input("the fetch was interrupted".into())),
            done = tokio::time::timeout(TIMEOUT, fetch) => done.unwrap_or_else(|_| {
                Err(ToolError::Input(format!("the fetch timed out after {}s", TIMEOUT.as_secs())))
            }),
        }
    }
}

#[cfg(test)]
mod tests;
