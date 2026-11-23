//! A host that configures its own client never has to install a TLS crypto
//! provider first (P2: reqwest is built without one, and building a client in
//! a process with none panicked inside reqwest, in the host's code).
//!
//! A test binary of its own: it needs a process in which nothing has
//! installed a provider yet.

#![cfg(all(feature = "reqwest", feature = "testing"))]

use std::time::Duration;

use haest_http::{Request, ReqwestClient, build_reqwest, reqwest_builder, testing::Server};

#[tokio::test]
async fn a_host_configured_client_builds_and_sends_with_no_provider_installed_first() {
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "this binary must start with no TLS provider installed"
    );
    // An empty 200 to every request.
    let server = Server::start([]);
    let url = server.url("/");

    let own = ReqwestClient::configured(|b| b.connect_timeout(Duration::from_secs(5)));
    let sent = tokio::spawn(async move { own.send(Request::get(url)).await });
    let res = sent.await.expect("sending never panics").unwrap();
    assert_eq!(res.status, 200);

    // A raw reqwest client (what the MCP client needs) builds too.
    assert!(build_reqwest(reqwest_builder()).is_ok());
}
