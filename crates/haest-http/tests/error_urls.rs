//! A request that fails says where it went (scheme, host, path), never what
//! a credential rides in: reqwest puts the request's URL in its error, and a
//! URL may carry a key in its query (`?api_key=`), a password in its
//! userinfo, a token in its fragment.

#![cfg(all(feature = "reqwest", feature = "testing"))]

use std::{error::Error, net::TcpListener};

use haest_http::{
    HttpError, Request, ReqwestClient,
    testing::{Respond, Server},
};

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

/// A password with an unescaped `/` reads to the URL parser as a host, a
/// port and a path: `127.0.0.1:<port>/pa55-SECRET@broker.example/...` meant
/// the user `127.0.0.1` and the password `<port>/pa55-SECRET`. The request
/// goes where the parser says, and its error names no part of that URL.
#[tokio::test]
async fn a_password_the_parser_took_for_a_path_is_not_in_the_error() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = closed.local_addr().unwrap();
    drop(closed);
    let url = format!("http://{addr}/pa55-SECRET@broker.example/v1/messages");
    let e = ReqwestClient::new()
        .send(Request::get(url))
        .await
        .unwrap_err();
    assert!(matches!(e, HttpError::Connect(_)), "{e:?}");
    let text = shown(&e);
    assert!(!text.contains("pa55-SECRET"), "{text}");
}

#[tokio::test]
async fn a_redirect_loop_is_reported_without_the_urls_credentials() {
    // Back to itself, the key still in the query.
    let back = ("location".into(), "/v1/messages?api_key=k3y-SECRET".into());
    let server =
        Server::answering(move |_| Respond::WithHeaders(307, vec![back.clone()], String::new()));
    let e = ReqwestClient::new()
        .send(Request::get(with_credentials(&server.url(""))))
        .await
        .unwrap_err();
    assert!(matches!(e, HttpError::Request(_)), "{e:?}");
    assert_no_credentials(&e);
}
