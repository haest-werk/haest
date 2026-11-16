//! WebFetch's client against a local server: it connects where the guard's
//! resolver says (never the system DNS), never to a blocked address, and
//! hands a redirect back instead of following it.

use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, TcpListener},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_http::{HttpClient, Request};

use super::{client, guarded_client};
use crate::net_guard::Resolve;

/// Every name resolves to the IPv4 loopback.
struct Loopback;

#[async_trait]
impl Resolve for Loopback {
    async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        Ok(vec![Ipv4Addr::LOCALHOST.into()])
    }
}

/// A server answering every request with `answer`: its port and the request
/// heads it was sent, lowercased.
fn server(answer: &'static [u8]) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let keep = seen.clone();
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
            keep.lock().unwrap().push(head);
            let _ = conn.write_all(answer);
        }
    });
    (port, seen)
}

const OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

#[tokio::test]
async fn it_connects_where_the_resolver_says_never_through_the_system_dns() {
    let (port, seen) = server(OK);
    // No DNS answers for .invalid: only the resolver's address reaches the server.
    let url = format!("http://pinned.invalid:{port}/");
    let res = client(Arc::new(Loopback), false)
        .send(Request::get(url))
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    assert!(seen.lock().unwrap()[0].contains("host: pinned.invalid:"));
}

#[tokio::test]
async fn a_name_that_resolves_to_a_blocked_address_when_connecting_is_never_reached() {
    let (port, seen) = server(OK);
    let url = format!("http://rebinding.test:{port}/");
    let sent = guarded_client(Arc::new(Loopback))
        .send(Request::get(url))
        .await;
    assert!(sent.is_err());
    assert!(seen.lock().unwrap().is_empty(), "the server was reached");
}

#[tokio::test]
async fn a_redirect_comes_back_as_the_answer_never_followed() {
    let (port, seen) = server(
        b"HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:1/next\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    );
    let url = format!("http://site.test:{port}/go");
    let res = client(Arc::new(Loopback), false)
        .send(Request::get(url))
        .await
        .unwrap();
    assert_eq!(res.status, 302);
    assert_eq!(res.header("location"), Some("http://127.0.0.1:1/next"));
    assert_eq!(seen.lock().unwrap().len(), 1);
}
