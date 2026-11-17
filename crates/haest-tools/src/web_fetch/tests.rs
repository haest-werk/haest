//! Ported from tools/web-fetch.test.ts (WebFetch), over a fake web: no network.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_core::{
    messages::MediaKind,
    tool::{Tool, ToolError, ToolOutput},
};
use haest_http::{Body, BoxError, Bytes, HttpClient, HttpError, Request, Response};
use serde_json::{Value, json};

use super::{WebFetchNet, WebFetchTool, markdown_of};
use crate::{
    net_guard::Resolve,
    testkit::{TestWorkspace, ctx, input},
};

const PUBLIC: &str = "93.184.215.14";

type Route = Box<dyn Fn() -> Response + Send + Sync>;

/// A path -> response table, every host resolving to `ips[host]` or PUBLIC.
#[derive(Default)]
struct FakeWeb {
    routes: HashMap<String, Route>,
    ips: HashMap<String, String>,
    hits: Mutex<Vec<String>>,
    resolved: Mutex<Vec<String>>,
}

/// A buffered answer with `headers`.
fn answer(status: u16, headers: &[(&str, &str)], body: impl Into<Body>) -> Response {
    headers
        .iter()
        .fold(Response::new(status, body), |res, (k, v)| {
            res.with_header(*k, *v)
        })
}

impl FakeWeb {
    fn route(mut self, url: &str, f: impl Fn() -> Response + Send + Sync + 'static) -> Self {
        self.routes.insert(url.into(), Box::new(f));
        self
    }
    fn ip(mut self, host: &str, ip: &str) -> Self {
        self.ips.insert(host.into(), ip.into());
        self
    }
    fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }
}

#[async_trait]
impl Resolve for FakeWeb {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, BoxError> {
        self.resolved.lock().unwrap().push(host.into());
        let ip = self.ips.get(host).map_or(PUBLIC, String::as_str);
        Ok(vec![ip.parse().unwrap()])
    }
}

#[async_trait]
impl HttpClient for FakeWeb {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        self.hits.lock().unwrap().push(req.url.clone());
        Ok(match self.routes.get(&req.url) {
            Some(route) => route(),
            None => Response::new(404, "not found"),
        })
    }
}

async fn fetch(web: &Arc<FakeWeb>, url: &str) -> Result<ToolOutput, ToolError> {
    let net = WebFetchNet::unguarded(web.clone(), web.clone());
    let ws = TestWorkspace::from_files([("/p/.keep", "")]);
    WebFetchTool::new(net)
        .execute(input(json!({"url": url})), &ctx(ws))
        .await
}

#[tokio::test]
async fn html_comes_back_as_markdown_with_scripts_and_styles_dropped() {
    let web = Arc::new(FakeWeb::default().route("https://site.test/", || {
        answer(
            200,
            &[("Content-Type", "text/html; charset=utf-8")],
            "<html><head><script>steal()</script><style>p{}</style></head><body><h1>Title</h1><p>Hi <a href=\"/x\">there</a></p></body></html>",
        )
    }));
    let out = fetch(&web, "https://site.test/").await.unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("# Title"));
    assert!(out.content.contains("[there](/x)"));
    assert!(!out.content.contains("steal"));
}

#[tokio::test]
async fn a_redirect_to_a_private_address_is_refused_before_it_is_fetched() {
    let web = Arc::new(
        FakeWeb::default()
            .route("https://site.test/go", || {
                answer(302, &[("location", "http://nas.test/admin")], "")
            })
            .ip("nas.test", "172.16.0.12"),
    );
    assert!(matches!(
        fetch(&web, "https://site.test/go").await,
        Err(ToolError::Input(_))
    ));
    assert_eq!(web.hits(), ["https://site.test/go"]);
}

#[tokio::test]
async fn a_redirect_loop_stops_at_the_hop_limit() {
    let web = Arc::new(FakeWeb::default().route("https://loop.test/", || {
        answer(301, &[("location", "/")], "")
    }));
    assert!(matches!(
        fetch(&web, "https://loop.test/").await,
        Err(ToolError::Input(_))
    ));
    assert_eq!(web.hits().len(), 6, "the first request and five redirects");
}

#[tokio::test]
async fn a_relative_redirect_is_followed_and_every_hop_is_checked_before_it_is_fetched() {
    let web = Arc::new(
        FakeWeb::default()
            .route("https://a.test/old", || {
                answer(301, &[("location", "https://b.test/new")], "")
            })
            .route("https://b.test/new", || {
                answer(307, &[("location", "page?x=1")], "")
            })
            .route("https://b.test/page?x=1", || {
                answer(200, &[("content-type", "application/json")], "{\"ok\":1}")
            })
            .ip("b.test", "1.1.1.1"),
    );
    let out = fetch(&web, "https://a.test/old").await.unwrap();
    assert!(out.content.contains("{\"ok\":1}"));
    assert_eq!(
        *web.resolved.lock().unwrap(),
        ["a.test", "b.test", "b.test"]
    );
}

#[tokio::test]
async fn a_body_past_the_byte_cap_is_refused() {
    let chunked = Arc::new(FakeWeb::default().route("https://big.test/", || {
        let chunks = (0..6).map(|_| Ok(Bytes::from(vec![b'x'; 1024 * 1024])));
        let body = Body::from_stream(futures::stream::iter(chunks));
        answer(200, &[("content-type", "text/plain")], body)
    }));
    assert!(matches!(
        fetch(&chunked, "https://big.test/").await,
        Err(ToolError::Input(_))
    ));
    let declared = Arc::new(FakeWeb::default().route("https://big.test/", || {
        answer(200, &[("content-length", "6291456")], "")
    }));
    assert!(matches!(
        fetch(&declared, "https://big.test/").await,
        Err(ToolError::Input(_))
    ));
}

#[tokio::test]
async fn an_image_is_attached_as_media_not_dumped_as_text() {
    let png = [0x89u8, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let web = Arc::new(
        FakeWeb::default().route("https://img.test/cat.png", move || {
            answer(200, &[("content-type", "image/png")], png.to_vec())
        }),
    );
    let out = fetch(&web, "https://img.test/cat.png").await.unwrap();
    assert_eq!(out.media.len(), 1);
    assert_eq!(out.media[0].kind, MediaKind::Image);
    assert_eq!(out.media[0].name, "cat.png");
}

#[tokio::test]
async fn an_http_error_status_comes_back_as_an_error_result_with_the_body() {
    let web = Arc::new(FakeWeb::default());
    let out = fetch(&web, "https://gone.test/").await.unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("not found"));
}

#[tokio::test]
async fn bad_urls_and_bytes_no_model_can_take_are_refused() {
    let web = Arc::new(FakeWeb::default().route("https://bin.test/x", || {
        answer(
            200,
            &[("content-type", "application/octet-stream")],
            vec![0u8; 4],
        )
    }));
    for bad in ["not a url", "file:///etc/passwd", "https://bin.test/x"] {
        assert!(
            matches!(fetch(&web, bad).await, Err(ToolError::Input(_))),
            "{bad}"
        );
    }
}

/// Never answers: only cancellation can end the call.
struct Hang;
#[async_trait]
impl HttpClient for Hang {
    async fn send(&self, _: Request) -> Result<Response, HttpError> {
        futures::future::pending().await
    }
}

#[tokio::test]
async fn an_interrupted_fetch_returns_at_once() {
    let net = WebFetchNet::unguarded(Arc::new(FakeWeb::default()), Arc::new(Hang));
    let c = ctx(TestWorkspace::from_files([("/p/.keep", "")]));
    c.cancel().cancel();
    let r = WebFetchTool::new(net)
        .execute(input(json!({"url": "https://slow.test/"})), &c)
        .await;
    assert!(matches!(r, Err(ToolError::Input(_))));
}

#[test]
fn markdown_keeps_structure_and_drops_embedded_code() {
    let md = markdown_of(
        "<h2>Sub</h2><ul><li>one</li></ul><pre><code>let x = 1;</code></pre><noscript>nojs</noscript><svg><text>pic</text></svg>",
    );
    assert!(md.contains("## Sub"));
    assert!(md.contains("one"));
    assert!(md.contains("```") && md.contains("let x = 1;"));
    assert!(!md.contains("nojs") && !md.contains("pic"));
}

/// The real client, against a closed loopback port: it must build (TLS provider
/// and all) and fail with an error, never panic. No traffic leaves the machine.
#[tokio::test]
async fn the_default_client_builds_and_reports_a_refused_connection() {
    let sent = WebFetchNet::default()
        .http
        .send(Request::get("http://127.0.0.1:1/"))
        .await;
    assert!(matches!(sent, Err(HttpError::Connect(_))));
}

#[test]
fn the_canonical_input_is_the_url() {
    let c = WebFetchTool::default().canonical_input(&input(json!({"url": "https://a.test/"})));
    assert_eq!(Value::Object(c), json!({"url": "https://a.test/"}));
}
