//! A base URL every dialect refuses to send to, and where each dialect's
//! path goes under one.

use super::*;
use crate::{AnthropicOptions, AnthropicProvider, ResponsesOptions, ResponsesProvider};

/// A base URL whose password holds an unescaped `/` reads to a URL parser
/// as another host (`user`, port 1234) with the rest of the password in the
/// path. Every dialect refuses to send to it, before the request leaves,
/// and the message says why and how to write it, showing no credential.
#[tokio::test]
async fn a_base_url_a_parser_misreads_is_never_sent_to() {
    let misread = "http://user:1234/s3cret@gw.example/v1";
    let t = FakeHttp::new([hello(), hello(), hello()]);
    let streams = [
        AnthropicProvider::new(
            AnthropicOptions::new("a", "k").with_base_url(misread),
            t.clone(),
        )
        .stream(request("m")),
        CompatProvider::new(
            CompatOptions::new("c", "k").with_base_url(misread),
            t.clone(),
        )
        .stream(request("m")),
        ResponsesProvider::new(
            ResponsesOptions::new("r", "k").with_base_url(misread),
            t.clone(),
        )
        .stream(request("m")),
    ];
    for stream in streams {
        let (events, msg) = collect(stream).await;
        assert_eq!(kinds(&events), ["done"]);
        assert_eq!(msg.stop_reason, StopReason::Error);
        let err = msg.error_message.unwrap_or_default();
        assert!(!err.contains("s3cret") && !err.contains("1234"), "{err}");
        assert!(
            err.contains("%2F") && err.contains("gw.example/v1"),
            "{err}"
        );
    }
    assert_eq!(t.requests().len(), 0, "a request went out");
}

/// A base URL that is a scheme with only slashes after it (`https:`,
/// `https:/`) passed the check: a parser skips the slashes and dials the
/// first part of the dialect's path as the host (`v1`, `chat`, `responses`),
/// sent the key. A provider a host builds itself, outside the registry,
/// sends nothing to it.
#[tokio::test]
async fn a_base_url_of_a_scheme_alone_is_never_sent_to() {
    for base in ["https:", "https:/", "HTTP:\\"] {
        let t = FakeHttp::new([hello(), hello(), hello()]);
        let streams = [
            AnthropicProvider::new(
                AnthropicOptions::new("a", "k").with_base_url(base),
                t.clone(),
            )
            .stream(request("m")),
            CompatProvider::new(CompatOptions::new("c", "k").with_base_url(base), t.clone())
                .stream(request("m")),
            ResponsesProvider::new(
                ResponsesOptions::new("r", "k").with_base_url(base),
                t.clone(),
            )
            .stream(request("m")),
        ];
        for stream in streams {
            let (events, msg) = collect(stream).await;
            assert_eq!(kinds(&events), ["done"], "{base}");
            assert_eq!(msg.stop_reason, StopReason::Error, "{base}");
        }
        let sent: Vec<String> = t.requests().into_iter().map(|r| r.url).collect();
        assert!(sent.is_empty(), "{base}: {sent:?}");
    }
}

/// A base URL's query (a gateway's key, an `api-version`) stays the query:
/// each dialect's path goes after the base's path, before it. It went after
/// the query, so the gateway saw the base path, and a key with the path
/// glued onto it.
#[tokio::test]
async fn a_base_urls_query_stays_after_the_dialects_path() {
    let base = "https://gw.example/anthropic?key=k3y";
    let refused = || FakeReply::status(400, "no");
    let t = FakeHttp::new([refused(), refused(), refused()]);
    let streams = [
        AnthropicProvider::new(
            AnthropicOptions::new("a", "k").with_base_url(base),
            t.clone(),
        )
        .stream(request("m")),
        CompatProvider::new(CompatOptions::new("c", "k").with_base_url(base), t.clone())
            .stream(request("m")),
        ResponsesProvider::new(
            ResponsesOptions::new("r", "k").with_base_url(base),
            t.clone(),
        )
        .stream(request("m")),
    ];
    for stream in streams {
        collect(stream).await;
    }
    let sent: Vec<String> = t.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(
        sent,
        [
            "https://gw.example/anthropic/v1/messages?key=k3y",
            "https://gw.example/anthropic/chat/completions?key=k3y",
            "https://gw.example/anthropic/responses?key=k3y",
        ]
    );
}
