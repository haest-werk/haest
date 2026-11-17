//! A host that configures its own client never has to install a TLS crypto
//! provider first (P2: reqwest is built without one, and building a client in
//! a process with none panicked inside reqwest, in the host's code).
//!
//! A test binary of its own: it needs a process in which nothing has
//! installed a provider yet.

#![cfg(feature = "reqwest")]

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

use haest_http::{HttpClient, Request, ReqwestClient, build_reqwest, reqwest_builder};

/// A local server answering every connection with an empty 200.
fn server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let _ = conn.read(&mut [0u8; 4096]);
            let _ = conn
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        }
    });
    url
}

#[tokio::test]
async fn a_host_configured_client_builds_and_sends_with_no_provider_installed_first() {
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "this binary must start with no TLS provider installed"
    );
    let url = server();

    let own = ReqwestClient::configured(|b| b.connect_timeout(Duration::from_secs(5)));
    let sent = tokio::spawn(async move { own.send(Request::get(url)).await });
    let res = sent.await.expect("sending never panics").unwrap();
    assert_eq!(res.status, 200);

    // A raw reqwest client (what the MCP client needs) builds too.
    assert!(build_reqwest(reqwest_builder()).is_ok());
}
