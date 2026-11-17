//! The default client's redirects: followed only within the request's origin,
//! and only when the request goes on as it was sent (307, 308). Another
//! origin is never sent the headers (an API key) or the body (a prompt).

#![cfg(feature = "reqwest")]

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};

use haest_http::{Request, ReqwestClient};

/// The requests one server got, head and body, lowercased.
type Seen = Arc<Mutex<Vec<String>>>;

/// A local server's listener and its base URL (its own origin: a port is part of one).
fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

/// Answer each request (one per connection) with `answer(path)`.
fn serve(listener: TcpListener, answer: impl Fn(&str) -> String + Send + 'static) -> Seen {
    let seen = Seen::default();
    let keep = seen.clone();
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let request = read_request(&mut conn);
            let path = request.split(' ').nth(1).unwrap_or_default().to_string();
            keep.lock().unwrap().push(request);
            let _ = conn.write_all(answer(&path).as_bytes());
        }
    });
    seen
}

/// One request: its head and its body, lowercased.
fn read_request(conn: &mut impl Read) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    let _ = conn.read_exact(&mut body);
    head + &String::from_utf8_lossy(&body).to_lowercase()
}

fn redirect(status: &str, to: &str) -> String {
    format!("HTTP/1.1 {status}\r\nlocation: {to}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
}

const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

/// A model call: a key in a header the redirect code does not know, a prompt in the body.
fn model_call(url: String) -> Request {
    Request::post(url)
        .with_header("x-api-key", "secret-key")
        .with_json(&serde_json::json!({"prompt": "private words"}))
}

#[tokio::test]
async fn a_redirect_to_another_origin_comes_back_and_that_origin_is_never_sent_anything() {
    let (other, other_base) = bind();
    let elsewhere = serve(other, |_| OK.to_string());
    for status in [
        "307 Temporary Redirect",
        "308 Permanent Redirect",
        "302 Found",
    ] {
        let (api, base) = bind();
        let to = format!("{other_base}/collect");
        serve(api, move |_| redirect(status, &to));
        let res = ReqwestClient::new()
            .send(model_call(format!("{base}/v1/messages")))
            .await
            .unwrap();
        assert!((300..400).contains(&res.status), "{status} was followed");
    }
    assert!(
        elsewhere.lock().unwrap().is_empty(),
        "another origin was sent the request"
    );
}

#[tokio::test]
async fn a_307_within_the_origin_is_followed_with_its_headers_and_body() {
    let (api, base) = bind();
    let seen = serve(api, |path| match path {
        "/old" => redirect("307 Temporary Redirect", "/new"),
        _ => OK.to_string(),
    });
    let res = ReqwestClient::new()
        .send(model_call(format!("{base}/old")))
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[1].starts_with("post /new"));
    assert!(seen[1].contains("x-api-key: secret-key") && seen[1].contains("private words"));
}

#[tokio::test]
async fn a_redirect_that_would_turn_the_request_into_a_bare_get_comes_back() {
    let (api, base) = bind();
    let seen = serve(api, |path| match path {
        "/old" => redirect("303 See Other", "/new"),
        _ => OK.to_string(),
    });
    let res = ReqwestClient::new()
        .send(model_call(format!("{base}/old")))
        .await
        .unwrap();
    assert_eq!(res.status, 303);
    assert_eq!(seen.lock().unwrap().len(), 1);
}
