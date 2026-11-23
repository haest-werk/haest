//! The default client reads no proxy from the environment (D15): with every
//! proxy variable pointing at a listener of its own, a request still goes
//! straight to its server, so a key never travels through a proxy haest was
//! not handed. A test binary of its own: it sets the process's variables.

#![cfg(feature = "reqwest")]

use haest_http::{Request, ReqwestClient, testing::Server};
use serde_json::json;

#[tokio::test]
#[expect(
    unsafe_code,
    reason = "setting the environment is unsafe, and the test sets a proxy"
)]
async fn the_default_client_goes_direct_whatever_the_proxy_variables_say() {
    // Each answers an empty 200.
    let (server, proxy) = (Server::start([]), Server::start([]));
    let proxy_url = proxy.url("");
    let url = server.url("/v1/messages");
    for name in ["NO_PROXY", "no_proxy", "REQUEST_METHOD"] {
        // SAFETY: the only test in this binary, and no thread of it reads the
        // environment while these are set.
        unsafe { std::env::remove_var(name) };
    }
    let proxies = ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"];
    for name in proxies.into_iter().chain(["ALL_PROXY"]) {
        // SAFETY: as above: the only test, and nothing reads the environment yet.
        unsafe { std::env::set_var(name, &proxy_url) };
    }

    let req = Request::post(url)
        .with_header("x-api-key", "secret")
        .with_json(&json!({}));
    let resp = ReqwestClient::new().send(req).await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(server.seen().len(), 1, "the request reached its server");
    assert!(proxy.seen().is_empty(), "and never the proxy");
}
