//! Where kern-tools' own HTTP clients connect: only where they were told, never
//! through a proxy the process environment names (PLAN D15: the library reads
//! nothing it was not handed).
//!
//! Its own test binary: every test here runs with HTTP_PROXY, HTTPS_PROXY and
//! ALL_PROXY pointing at a listener that must see nothing, which no other
//! test may inherit.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex, OnceLock},
};

use kern_tools::image::{ImageHttp, ReqwestImageHttp};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// The request heads one listener was sent, lowercased.
type Seen = Arc<Mutex<Vec<String>>>;

/// A local HTTP server answering every request `200 {}`: its base URL and what it got.
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

/// Whether any request in `seen` was for `url` (a proxy is sent the whole URL).
fn asked_for(seen: &Seen, url: &str) -> bool {
    seen.lock().unwrap().iter().any(|head| head.contains(url))
}

/// Point every proxy variable at a listener that must see nothing; once per binary.
fn proxy_in_env() -> Seen {
    static PROXY: OnceLock<Seen> = OnceLock::new();
    PROXY
        .get_or_init(|| {
            let (url, seen) = listener();
            // SAFETY: set once, before any test of this binary builds a client
            // (each calls this first and waits for it); nothing else here
            // touches the environment.
            unsafe {
                std::env::remove_var("NO_PROXY");
                std::env::remove_var("no_proxy");
                for var in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
                    std::env::set_var(var, &url);
                }
            }
            seen
        })
        .clone()
}

#[tokio::test]
async fn the_image_api_and_its_key_go_straight_to_the_api_never_through_an_env_proxy() {
    let proxy = proxy_in_env();
    let (api, direct) = listener();
    let http = ReqwestImageHttp;
    let cancel = CancellationToken::new();
    let posted = http
        .post_json(
            &format!("{api}/images"),
            "image-api-key",
            &json!({"model": "m"}),
            cancel.clone(),
        )
        .await;
    let listed = http.get(&format!("{api}/models"), cancel).await;

    assert!(
        !asked_for(&proxy, &api),
        "the env proxy was sent the API's requests"
    );
    assert_eq!(posted.unwrap().status, 200);
    assert_eq!(listed.unwrap().status, 200);
    let direct = direct.lock().unwrap();
    assert_eq!(direct.len(), 2);
    assert!(direct[0].contains("authorization: bearer image-api-key"));
}
