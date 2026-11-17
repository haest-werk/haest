//! A request that fails says where it went (scheme, host, path), never what
//! a credential rides in: reqwest puts the request's URL in its error, and a
//! URL may carry a key in its query (`?api_key=`), a password in its
//! userinfo, a token in its fragment.

#![cfg(feature = "reqwest")]

use std::{
    error::Error,
    io::{Read, Write},
    net::TcpListener,
};

use haest_http::{HttpError, Request, ReqwestClient};

/// The error's text with every cause under it, and its Debug output: all a
/// log or a model could be shown of it.
fn shown(e: &HttpError) -> String {
    let mut text = format!("{e:?}\n{e}");
    let mut cause = e.source();
    while let Some(c) = cause {
        text.push_str(&format!("\n{c}"));
        cause = c.source();
    }
    text
}

fn assert_no_credentials(e: &HttpError) {
    let text = shown(e);
    for secret in ["k3y-SECRET", "pa55-SECRET", "frag-SECRET"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert!(
        text.contains("/v1/messages"),
        "where it went is gone: {text}"
    );
}

/// A URL with a credential in each place one rides in.
fn with_credentials(base: &str) -> String {
    let base = base.replace("http://", "http://user:pa55-SECRET@");
    format!("{base}/v1/messages?api_key=k3y-SECRET#frag-SECRET")
}

#[tokio::test]
async fn a_server_that_cannot_be_reached_is_reported_without_the_urls_credentials() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let e = ReqwestClient::new()
        .send(Request::get(with_credentials(&base)))
        .await
        .unwrap_err();
    assert!(matches!(e, HttpError::Connect(_)), "{e:?}");
    assert_no_credentials(&e);
}

#[tokio::test]
async fn a_redirect_loop_is_reported_without_the_urls_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            // Back to itself, the key still in the query.
            let answer = "HTTP/1.1 307 Temporary Redirect\r\nlocation: /v1/messages?api_key=k3y-SECRET\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            let _ = conn.write_all(answer.as_bytes());
        }
    });
    let e = ReqwestClient::new()
        .send(Request::get(with_credentials(&base)))
        .await
        .unwrap_err();
    assert!(matches!(e, HttpError::Request(_)), "{e:?}");
    assert_no_credentials(&e);
}
