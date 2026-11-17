//! The default client reads no proxy from the environment (D15): with every
//! proxy variable pointing at a listener of its own, a request still goes
//! straight to its server, so a key never travels through a proxy haest was
//! not handed. A test binary of its own: it sets the process's variables.

#![cfg(feature = "reqwest")]

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};

use haest_http::{HttpClient, Request, ReqwestClient};
use serde_json::json;

/// Answers every connection with an empty 200, and says each time it was reached.
fn serve(listener: TcpListener) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let _ = tx.send(());
            let _ = conn.read(&mut [0u8; 4096]);
            let _ = conn
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        }
    });
    rx
}

#[tokio::test]
#[expect(
    unsafe_code,
    reason = "setting the environment is unsafe, and the test sets a proxy"
)]
async fn the_default_client_goes_direct_whatever_the_proxy_variables_say() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let url = format!("http://{}/v1/messages", server.local_addr().unwrap());
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
    let (reached, proxied) = (serve(server), serve(proxy));

    let req = Request::post(url)
        .with_header("x-api-key", "secret")
        .with_json(&json!({}));
    let resp = ReqwestClient::new().send(req).await.unwrap();
    assert_eq!(resp.status, 200);
    assert!(
        reached.recv_timeout(Duration::from_secs(2)).is_ok(),
        "the request reached its server"
    );
    assert!(proxied.try_recv().is_err(), "and never the proxy");
}
