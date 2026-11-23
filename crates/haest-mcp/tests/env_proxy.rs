//! An HTTP MCP server is reached directly, its headers (an `Authorization`
//! among them) sent to it alone, never through a proxy the process environment
//! names (PLAN D15: the library reads nothing it was not handed).
//!
//! Its own test binary: it sets HTTP_PROXY, HTTPS_PROXY and ALL_PROXY, which no
//! other test may inherit.

use haest_http::testing::{Respond, Server};
use haest_mcp::{McpHub, McpServerConfig, McpTransport};

/// A local HTTP server answering every request `200 {}` (no MCP server, so the
/// connect fails at once).
fn listener() -> Server {
    Server::answering(|_| Respond::With(200, "{}".into()))
}

#[tokio::test]
#[expect(
    unsafe_code,
    reason = "setting the environment is unsafe, and the test sets a proxy"
)]
async fn an_http_server_and_its_headers_are_never_sent_through_an_env_proxy() {
    let (proxy, server) = (listener(), listener());
    for var in ["NO_PROXY", "no_proxy"] {
        // SAFETY: this binary's only test, and it sets them before any client exists.
        unsafe { std::env::remove_var(var) };
    }
    for var in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        // SAFETY: as above: the only test, and no client exists yet.
        unsafe { std::env::set_var(var, proxy.url("")) };
    }
    let http =
        McpTransport::http(server.url("/mcp")).with_header("Authorization", "Bearer mcp-key");
    let cfg = McpServerConfig::new("remote", http);
    let hub = McpHub::new();
    hub.connect(&[cfg]).await;

    assert!(
        proxy.seen().is_empty(),
        "the env proxy was sent the MCP server's requests"
    );
    assert_eq!(proxy.connections(), 0, "the env proxy was connected to");
    let direct = server.seen();
    assert!(!direct.is_empty(), "the MCP server was never reached");
    assert_eq!(direct[0].header("authorization"), Some("Bearer mcp-key"));
}
