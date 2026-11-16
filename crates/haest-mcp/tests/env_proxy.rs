//! An HTTP MCP server is reached directly, its headers (an `Authorization`
//! among them) sent to it alone, never through a proxy the process environment
//! names (PLAN D15: the library reads nothing it was not handed).
//!
//! Its own test binary: it sets HTTP_PROXY, HTTPS_PROXY and ALL_PROXY, which no
//! other test may inherit.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};

use haest_mcp::{McpHub, McpServerConfig, McpTransport};

/// The request heads one listener was sent, lowercased.
type Seen = Arc<Mutex<Vec<String>>>;

/// A local HTTP server answering every request `200 {}` (no MCP server, so the
/// connect fails at once): its base URL and what it got.
fn listener() -> (String, Seen) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen = Seen::default();
    let keep = seen.clone();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            keep.lock().unwrap().push(read_request(&mut s));
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            );
        }
    });
    (url, seen)
}

/// The head of one request (its body read and dropped, so closing never resets).
fn read_request(s: &mut impl Read) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") && s.read(&mut byte).unwrap_or(0) == 1 {
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let _ = s.read_exact(&mut vec![0; len]);
    head
}

#[tokio::test]
async fn an_http_server_and_its_headers_are_never_sent_through_an_env_proxy() {
    let (proxy, via_proxy) = listener();
    let (server, direct) = listener();
    // SAFETY: this binary's only test, and it sets them before any client exists.
    unsafe {
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
        for var in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            std::env::set_var(var, &proxy);
        }
    }
    let http =
        McpTransport::http(format!("{server}/mcp")).with_header("Authorization", "Bearer mcp-key");
    let cfg = McpServerConfig::new("remote", http);
    let hub = McpHub::new();
    hub.connect(&[cfg], &|_| {}).await;

    assert!(
        via_proxy.lock().unwrap().is_empty(),
        "the env proxy was sent the MCP server's requests"
    );
    let direct = direct.lock().unwrap();
    assert!(!direct.is_empty(), "the MCP server was never reached");
    assert!(direct[0].contains("authorization: bearer mcp-key"));
}
