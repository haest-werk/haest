//! The default client's redirects: followed only within the request's origin,
//! and only when the request goes on as it was sent (307, 308). Another
//! origin is never sent the headers (an API key) or the body (a prompt).

#![cfg(all(feature = "reqwest", feature = "testing"))]

use haest_http::{
    Request, ReqwestClient,
    testing::{Respond, Server},
};

/// `status`, sending the client to `to`.
fn redirect(status: u16, to: &str) -> Respond {
    Respond::WithHeaders(status, vec![("location".into(), to.into())], String::new())
}

/// A model call: a key in a header the redirect code does not know, a prompt in the body.
fn model_call(url: String) -> Request {
    Request::post(url)
        .with_header("x-api-key", "secret-key")
        .with_json(&serde_json::json!({"prompt": "private words"}))
}

#[tokio::test]
async fn a_redirect_to_another_origin_comes_back_and_that_origin_is_never_sent_anything() {
    // Another port is another origin.
    let elsewhere = Server::start([]);
    for status in [307, 308, 302] {
        let to = elsewhere.url("/collect");
        let api = Server::answering(move |_| redirect(status, &to));
        let res = ReqwestClient::new()
            .send(model_call(api.url("/v1/messages")))
            .await
            .unwrap();
        assert!((300..400).contains(&res.status), "{status} was followed");
    }
    assert!(
        elsewhere.seen().is_empty(),
        "another origin was sent the request"
    );
}

#[tokio::test]
async fn a_307_within_the_origin_is_followed_with_its_headers_and_body() {
    let api = Server::answering(|seen| match seen.path() {
        "/old" => redirect(307, "/new"),
        _ => Respond::With(200, String::new()),
    });
    let res = ReqwestClient::new()
        .send(model_call(api.url("/old")))
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    let seen = api.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].line, "POST /new HTTP/1.1");
    assert_eq!(seen[1].header("x-api-key"), Some("secret-key"));
    assert!(String::from_utf8_lossy(&seen[1].body).contains("private words"));
}

#[tokio::test]
async fn a_redirect_that_would_turn_the_request_into_a_bare_get_comes_back() {
    let api = Server::answering(|seen| match seen.path() {
        "/old" => redirect(303, "/new"),
        _ => Respond::With(200, String::new()),
    });
    let res = ReqwestClient::new()
        .send(model_call(api.url("/old")))
        .await
        .unwrap();
    assert_eq!(res.status, 303);
    assert_eq!(api.seen().len(), 1);
}
