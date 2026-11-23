//! Where haest-tools' default HTTP clients connect: only where they were told,
//! never through a proxy the process environment names (PLAN D15: the library
//! reads nothing it was not handed; a proxy would be sent the Image API's key
//! and every page WebFetch reads).
//!
//! Its own test binary: every test here runs with HTTP_PROXY, HTTPS_PROXY and
//! ALL_PROXY pointing at a listener that must see nothing, which no other
//! test may inherit.

use std::{
    net::IpAddr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use haest_core::{
    tool::{Tool, ToolContext, input},
    workspace::ShutWorkspace,
};
use haest_http::testing::{Respond, Server};
use haest_tools::{
    GenerateImageOptions, GenerateImageTool, Resolve, WebFetchNet, WebFetchTool, http::BoxError,
};
use serde_json::json;

/// A local HTTP server answering every request `200 {}`.
fn listener() -> Server {
    Server::answering(|_| Respond::With(200, "{}".into()))
}

/// Whether `server` was sent a request for `url` (a proxy is sent the whole URL).
fn asked_for(server: &Server, url: &str) -> bool {
    server.seen().iter().any(|seen| seen.path().contains(url))
}

/// Point every proxy variable at a listener that must see nothing; once per
/// binary, and it lives as long as the binary does.
#[expect(
    unsafe_code,
    reason = "setting the environment is unsafe, and these tests set a proxy"
)]
fn proxy_in_env() -> &'static Server {
    static PROXY: OnceLock<Server> = OnceLock::new();
    PROXY.get_or_init(|| {
        let proxy = listener();
        let url = proxy.url("");
        for var in ["NO_PROXY", "no_proxy"] {
            // SAFETY: set once, before any test of this binary builds a
            // client (each calls this first and waits for it); nothing
            // else here touches the environment.
            unsafe { std::env::remove_var(var) };
        }
        for var in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            // SAFETY: as above: once, before any client exists.
            unsafe { std::env::set_var(var, &url) };
        }
        proxy
    })
}

#[tokio::test]
async fn the_image_api_and_its_key_go_straight_to_the_api_never_through_an_env_proxy()
-> Result<(), BoxError> {
    let proxy = proxy_in_env();
    let api = listener();
    let opts = GenerateImageOptions::new("image-api-key")
        .with_base_url(api.url(""))
        .with_models("m", [] as [&str; 0]);
    let tool = GenerateImageTool::new(opts);
    // The model list (`{}`, unreadable: nothing is checked), then the image
    // request: `{}` is no image, refused only after the API answered.
    let posted = tool
        .execute(
            input(json!({"prompt": "p"}))?,
            &ToolContext::new("/", ShutWorkspace),
        )
        .await;

    assert!(
        !asked_for(proxy, &api.url("")),
        "the env proxy was sent the API's requests"
    );
    assert!(posted.is_err());
    let direct = api.seen();
    assert_eq!(direct.len(), 2, "the model list, then the image request");
    assert!(
        direct[0].line.starts_with("GET /images/models"),
        "{direct:?}"
    );
    assert_eq!(
        direct[1].header("authorization"),
        Some("Bearer image-api-key")
    );
    Ok(())
}

/// The first lookup (the guard's check) answers a public address; every later
/// one (the client's own, when it connects) fails, so nothing is dialled.
struct PublicOnce(AtomicUsize);

#[async_trait]
impl Resolve for PublicOnce {
    async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, BoxError> {
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(vec!["93.184.215.14".parse().unwrap()]),
            _ => Err("no answer the second time".into()),
        }
    }
}

#[tokio::test]
async fn webfetch_never_goes_through_an_env_proxy() {
    let proxy = proxy_in_env();
    let url = "http://site.test:8080/page";
    let resolve = Arc::new(PublicOnce(AtomicUsize::new(0)));
    let tool = WebFetchTool::new(WebFetchNet::new(resolve.clone()));
    let ctx = ToolContext::new("/", ShutWorkspace);
    let input = json!({"url": url}).as_object().unwrap().clone();
    // Through the proxy this would be answered at once, and the name never
    // resolved here; direct, the client's own lookup fails.
    let fetched = tool.execute(input, &ctx).await;
    assert!(fetched.is_err(), "something answered");
    assert_eq!(
        resolve.0.load(Ordering::SeqCst),
        2,
        "the client resolved the name itself"
    );
    assert!(
        !asked_for(proxy, url),
        "the env proxy was sent WebFetch's request"
    );
}
