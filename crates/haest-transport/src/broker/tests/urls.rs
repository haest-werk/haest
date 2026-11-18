//! The broker URLs `attach` takes, and the URL the link dials for each.

use haest_wire::Origin;

use super::{Broker, FAST, TestBroker, ty};
use crate::{
    Grant, TransportError,
    broker::{dialable, dialed_url},
    testing::{answers, hub, session},
};

/// A URL that starts with `ws://` or `wss://` but that no dial can reach (a
/// space, a host outside ASCII, a `\`, a port past 65535) was attached, then
/// dialed in vain for good: every dial failed the same way before any socket
/// opened (or, for the port, went to the scheme's default port), and the
/// link dropped each error, so the host never heard. `attach` refuses it, as
/// it does a URL with another scheme, and dials nothing.
#[tokio::test]
async fn a_url_no_dial_can_reach_is_refused_at_attach() {
    for url in [
        "ws://broker .example/agents",
        "wss://b\u{fc}cher.example/agents",
        "ws://agent:s3cret@127.0.0.1:9\\agents",
        "ws://127.0.0.1:9/my agents",
        "ws://127.0.0.1:70000/agents",
        "wss://agent:s3cret@127.0.0.1:84433/agents",
        "ws://[::1]:65536",
    ] {
        let (s, _) = session(&answers(&[]), hub());
        let link = Broker::new(url)
            .with_backoff(FAST)
            .attach(s.clone(), Grant::watch(Origin::Agent));
        let refused = link.expect_err(url);
        assert!(
            matches!(refused, TransportError::Undialable(_)),
            "{url}: {refused:?}"
        );
        let said = haest_core::error::report(&refused);
        assert!(!said.contains("s3cret"), "{said}");
        s.close().await;
    }
    // No host: refused as `check_endpoint_url` refuses it.
    let (s, _) = session(&answers(&[]), hub());
    let refused = Broker::new("wss:///agents").attach(s.clone(), Grant::watch(Origin::Agent));
    assert!(
        matches!(refused, Err(TransportError::UrlRefused(_))),
        "{refused:?}"
    );
    s.close().await;
    assert!(dialable("ws://127.0.0.1:65535/agents"), "the largest port");
    // Dialed: an IPv6 host, a userinfo, no path, a query or a fragment right
    // after the host.
    for url in [
        "ws://[::1]:9/agents",
        "ws://agent:s3cret@127.0.0.1:9",
        "ws://127.0.0.1:9?v=1",
        "ws://127.0.0.1:9#f",
    ] {
        let (s, _) = session(&answers(&[]), hub());
        let link = Broker::new(url).attach(s.clone(), Grant::watch(Origin::Agent));
        assert!(link.is_ok(), "{url}: {:?}", link.err());
        drop(link);
        s.close().await;
    }
}

/// A URL with nothing between `//` (or the userinfo's `@`) and its port
/// (`wss://:8443/agents`, what `wss://${HOST}:8443/agents` makes of an empty
/// variable) was attached, and every dial failed to look up the empty host
/// name: the link dialed in vain for good, and the host never heard.
/// `attach` refuses it, as it does `wss:///agents`, and dials nothing.
#[tokio::test]
async fn a_url_with_no_host_before_its_port_is_refused() {
    for url in [
        "wss://:8443/agents",
        "ws://:9",
        "ws://agent:s3cret@:9/agents",
        "ws://@:9/agents",
        "ws://[]:9/agents",
        "wss://\r:443/agents",
    ] {
        let (s, _) = session(&answers(&[]), hub());
        let refused = Broker::new(url)
            .with_backoff(FAST)
            .attach(s.clone(), Grant::watch(Origin::Agent));
        let refused = refused.expect_err(url);
        assert!(
            matches!(refused, TransportError::UrlRefused(_)),
            "{url:?}: {refused:?}"
        );
        let said = haest_core::error::report(&refused);
        assert!(!said.contains("s3cret"), "{said}");
        s.close().await;
    }
}

/// A broker URL with a space, a tab or a CR around or in it is refused
/// before any dial. One that led with it was refused as `Url` ("not a ws://
/// URL"), and the URL in a refusal carried a CR as it is, which wipes the
/// line a host prints it on. Each is `Undialable`, and the URL shows with
/// its control characters escaped.
#[tokio::test]
async fn a_url_with_whitespace_is_undialable_and_shows_it() {
    for url in [
        " ws://127.0.0.1:9/agents",
        "\tws://127.0.0.1:9/agents",
        "ws://127.0.0.1:9/agents\r",
        "ws://127.0.0.1:9/agents\r\n",
        "ws://127.0.0.1:9/my\tagents",
    ] {
        let (s, _) = session(&answers(&[]), hub());
        let refused = Broker::new(url).attach(s.clone(), Grant::watch(Origin::Agent));
        let refused = refused.expect_err(url);
        let TransportError::Undialable(shown) = &refused else {
            panic!("{url:?}: {refused:?}");
        };
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        s.close().await;
    }
    // Not a ws:// URL, its control character shown escaped too.
    let (s, _) = session(&answers(&[]), hub());
    let refused =
        Broker::new("http://127.0.0.1:9/agents\r").attach(s.clone(), Grant::watch(Origin::Agent));
    let Err(TransportError::Url(shown)) = &refused else {
        panic!("{refused:?}");
    };
    assert!(!shown.chars().any(char::is_control), "{shown:?}");
    s.close().await;
}

/// The URL the link dials: no userinfo, and `/` for a path when it has none.
#[test]
fn the_link_dials_the_url_without_its_userinfo_and_with_a_path() {
    for (url, dialed) in [
        (
            "wss://h.example/agents?v=1#f",
            "wss://h.example/agents?v=1#f",
        ),
        ("wss://h.example", "wss://h.example/"),
        ("wss://h.example?v=1", "wss://h.example/?v=1"),
        ("wss://h.example:8443#f", "wss://h.example:8443/#f"),
        ("ws://agent:s3cret@[::1]:9/agents", "ws://[::1]:9/agents"),
        (
            "wss://me@corp.example:s3@cret@h.example?v=1",
            "wss://h.example/?v=1",
        ),
        ("ws://@h.example/agents", "ws://h.example/agents"),
    ] {
        assert_eq!(dialed_url(url), dialed, "{url}");
    }
}

/// A broker URL with a query right after the host (`ws://h:1?v=1`) never got
/// through the handshake: tungstenite wrote `GET ?v=1 HTTP/1.1`, which a
/// server refuses (RFC 6455, section 3: the resource name starts with `/`).
/// The link dials it with `/` for its path.
#[tokio::test]
async fn a_url_with_a_query_and_no_path_is_dialed_with_a_slash() {
    let mut broker = TestBroker::start(None).await;
    let url = broker.url.replace("/agents", "?v=1");
    let (s, _) = session(&answers(&[]), hub());
    let attached = Broker::new(&url)
        .with_backoff(FAST)
        .attach(s.clone(), Grant::watch(Origin::Agent))
        .unwrap();
    let mut side = broker.link().await;
    let hello = side.next().await.expect("a frame");
    assert_eq!(ty(&hello), "hello");
    assert_eq!(broker.paths.lock().unwrap().as_slice(), ["/?v=1"]);
    attached.detach();
    s.close().await;
}
