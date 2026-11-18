//! `check_endpoint_url`'s verdicts, the host a parser dials for each, and
//! `join_path`.

use super::*;
use crate::redacted_url;

/// Refused: an `@` past where a parser ends the host, whatever ends it.
const MISREAD: [&str; 10] = [
    "http://user:1234/secret@broker.example/agents",
    "wss://agent:abc/def+ghi=@broker.example/agents",
    "https://user:p?ss@mcp.example/mcp",
    "https://user:p#ss@mcp.example/mcp",
    "http://user:12\\34@gw.example/v1",
    "https://u:p@s/s@host/x",
    // An `@` meant for the path, the query or the fragment.
    "https://registry.example/@scope/pkg",
    "https://mcp.example/mcp?email=a@b.example",
    "https://mcp.example/mcp#a@b",
    // No scheme: a parser reads `user` as one, or fails.
    "user:p/ss@host/x",
];

/// Passed: a URL parser reads the host after the last `@` of the
/// userinfo, or there is none.
const READ_RIGHT: [&str; 8] = [
    "https://user:p%2Fss@broker.example/agents",
    "https://api.example.com/v1",
    "https://registry.example/%40scope/pkg",
    "https://mcp.example/mcp?email=a%40b.example",
    "https://u:p@ss@host/v1",
    "wss://agent:s3cret@broker.example:8443/agents?v=1#f",
    "http://[::1]:8080/v1",
    "",
];

/// Refused: no host after `scheme://` (an empty variable,
/// `https://${HOST}/v1`). A parser takes the first part of the path for
/// one: `https:///anthropic` dials the host `anthropic`.
const NO_HOST: [&str; 6] = [
    "https://",
    "https:///anthropic",
    "https://?key=k",
    "http:///openrouter/api/v1",
    "wss:///agents",
    "HTTPS://#f",
];

/// Refused too: an authority that is only a userinfo and a port
/// (`wss://${HOST}:8443/agents` with the variable empty), and a missing
/// host behind a space, tab, CR or LF, which a URL parser drops before it
/// reads the URL (a CRLF `.env` keeps the `\r` of an empty `HOST=`).
const NO_HOST_PART: [&str; 12] = [
    "wss://:8443/agents",
    "http://:8080/v1",
    "wss://:443",
    "ws://agent:s3cret@:9/agents",
    "ws://@:9/agents",
    "https://u:p@/v1",
    "ws://[]:9/agents",
    "https://\r:8443/v1",
    " https:///anthropic",
    "\thttps:///anthropic",
    "https://\t/anthropic",
    "https://\n/anthropic",
];

/// Passed: a host, with the spaces and line ends a parser drops around or
/// inside the URL.
const HOST_THERE: [&str; 7] = [
    "https://h",
    "https://@h/v1",
    "wss://u:p@h:1/x",
    "http://[::1]:8080/v1",
    "https://gw.example/v1\r",
    " https://gw.example/v1 \n",
    "\thttps://u:p@gw.example/v1",
];

/// Refused too: `http`, `https`, `ws` or `wss` with nothing after its `:`
/// but `/` and `\` up to its query, its fragment or its end. A parser skips
/// them, so the first part of a path joined under it is the host: `https:`
/// and `v1/messages` dial the host `v1`.
const BARE_SCHEME: [&str; 8] = [
    "https:",
    "https:/",
    "https:\\",
    "HTTP:\\\\",
    "wss:?x",
    "http:#f",
    "https:\r",
    " https:/ ",
];

/// Passed: no `//`, but a host after the scheme, which a parser dials.
const HOST_WITHOUT_SLASHES: [&str; 3] = [
    "http:localhost:8080/v1",
    "https:/anthropic/v1",
    "http:\\gw.example\\v1",
];

/// A scheme with only slashes after it passed: the check read no authority
/// without `scheme://`, and a path joined under it dialed its first part
/// as the host, with the key. It is refused, under any path too; a scheme
/// with a host after it and no `//` still passes.
#[test]
fn a_scheme_with_only_slashes_after_it_is_refused() {
    for url in BARE_SCHEME {
        let refused = check_endpoint_url(url);
        assert!(
            matches!(&refused, Err(e) if e.why == Why::NoHost),
            "{url:?}: {refused:?}"
        );
        let joined = join_path(url, "v1/messages");
        assert!(check_endpoint_url(&joined).is_err(), "{joined:?}");
    }
    for url in HOST_WITHOUT_SLASHES {
        assert_eq!(check_endpoint_url(url), Ok(()), "{url:?}");
        let joined = join_path(url, "v1/messages");
        assert_eq!(check_endpoint_url(&joined), Ok(()), "{joined:?}");
    }
}

/// A URL with no host passed, and the joined URL dialed the first part
/// of the path as the host. It is refused, and stays refused under a
/// path, so the check on a request's URL agrees with the one on its base.
#[test]
fn a_url_with_no_host_is_refused_with_any_path_under_it() {
    for url in NO_HOST {
        let said = check_endpoint_url(url).expect_err(url).to_string();
        assert!(said.contains("has no host"), "{url}: {said}");
        let joined = join_path(url, "v1/messages");
        assert!(check_endpoint_url(&joined).is_err(), "{joined}");
    }
    // A host, or `@` with one after it, is a host.
    for url in ["https://h", "https://@h/v1", "wss://u:p@h:1/x"] {
        assert_eq!(check_endpoint_url(url), Ok(()), "{url}");
    }
}

/// An authority with only a userinfo or a port, or a host left out behind
/// whitespace a parser drops, passed: the whole authority was not empty, or
/// the check read the space as part of it. Each is refused as having no
/// host, under any path too; a URL with a host around that whitespace
/// still passes.
#[test]
fn an_authority_with_no_host_in_it_is_refused() {
    for url in NO_HOST_PART {
        let refused = check_endpoint_url(url);
        assert!(
            matches!(&refused, Err(e) if e.why == Why::NoHost),
            "{url:?}: {refused:?}"
        );
        let joined = join_path(url, "v1/messages");
        assert!(check_endpoint_url(&joined).is_err(), "{joined:?}");
    }
    for url in HOST_THERE {
        assert_eq!(check_endpoint_url(url), Ok(()), "{url:?}");
        let joined = join_path(url, "v1/messages");
        assert_eq!(check_endpoint_url(&joined), Ok(()), "{joined:?}");
    }
}

/// A refused URL's message carried its control characters as they are: a
/// CR (a CRLF `.env`) printed to a terminal moved the cursor to the start
/// of the line, and the rest of the message wrote over the URL. They show
/// escaped.
#[test]
fn a_refused_urls_control_characters_show_escaped() {
    for url in [
        "https://\r/anthropic",
        "wss://:8443/agents\u{7}",
        "https:\u{1b}",
    ] {
        let e = check_endpoint_url(url).expect_err(url);
        assert!(!e.url().chars().any(char::is_control), "{:?}", e.url());
        assert!(!e.to_string().chars().any(char::is_control), "{e:?}");
    }
}

/// An `@` in a path or a query is refused because it cannot be told from
/// one that ends a password. A parser reads such a URL right, and the
/// refusal does not say it misreads it.
#[test]
fn the_refusal_of_an_at_in_a_path_or_query_does_not_say_a_parser_misreads_it() {
    for url in &MISREAD[6..9] {
        let said = check_endpoint_url(url).expect_err(url).to_string();
        assert!(!said.contains("misreads"), "{said}");
        assert!(
            said.contains("cannot tell from one that ends a password"),
            "{said}"
        );
    }
}

#[test]
fn a_path_goes_after_the_bases_path_and_before_its_query_and_fragment() {
    let cases = [
        ("https://gw.example/v1", "https://gw.example/v1/x/y"),
        ("https://gw.example/v1/", "https://gw.example/v1/x/y"),
        ("https://gw.example/v1//", "https://gw.example/v1/x/y"),
        ("https://gw.example", "https://gw.example/x/y"),
        (
            "https://gw.example/v1?key=k",
            "https://gw.example/v1/x/y?key=k",
        ),
        (
            "https://gw.example/v1/?key=k&v=2",
            "https://gw.example/v1/x/y?key=k&v=2",
        ),
        ("https://gw.example?key=k", "https://gw.example/x/y?key=k"),
        ("https://gw.example/v1#f", "https://gw.example/v1/x/y#f"),
        (
            "https://gw.example/v1?key=a/b#f/g",
            "https://gw.example/v1/x/y?key=a/b#f/g",
        ),
        (
            "https://u:p@ss@gw.example:8443/v1?key=k",
            "https://u:p@ss@gw.example:8443/v1/x/y?key=k",
        ),
    ];
    for (base, joined) in cases {
        assert_eq!(join_path(base, "x/y"), joined, "{base}");
    }
}

/// The check's verdict on a base URL holds for every path under it, so
/// checking the base or the URL sent is the same.
#[test]
fn a_path_under_a_url_keeps_the_checks_verdict() {
    for url in MISREAD {
        let joined = join_path(url, "v1/messages");
        assert!(check_endpoint_url(&joined).is_err(), "{joined}");
    }
    for url in READ_RIGHT {
        let joined = join_path(url, "v1/messages");
        assert_eq!(check_endpoint_url(&joined), Ok(()), "{joined}");
    }
}

#[test]
fn a_url_with_an_at_past_its_host_is_refused() {
    for url in MISREAD {
        let e = check_endpoint_url(url).expect_err(url);
        assert_eq!(e.url(), redacted_url(url), "{url}");
    }
    for url in READ_RIGHT {
        assert_eq!(check_endpoint_url(url), Ok(()), "{url}");
    }
}

/// The refusal says why and how to write it, and shows no piece of the
/// password whatever character in it a parser took for the host's end.
#[test]
fn the_refusal_says_how_to_write_it_and_shows_no_credential() {
    // The user, the password's pieces, a query value.
    let pieces = ["Z9y8", "K1x9", "Q7z2", "W3v4", "R8t0"];
    let passwords = [
        "K1x9/Q7z2",
        "K1x9?Q7z2",
        "K1x9#Q7z2",
        "K1x9\\Q7z2",
        "K1x9/Q7z2@W3v4",
    ];
    for pw in passwords {
        let url = format!("wss://Z9y8:{pw}@broker.example/agents?key=R8t0");
        let said = check_endpoint_url(&url).expect_err(&url).to_string();
        for secret in pieces {
            assert!(!said.contains(secret), "{secret} of {url}: {said}");
        }
        for hint in [
            "%2F",
            "%3F",
            "%23",
            "%5C",
            "%40",
            "a URL parser ends the host",
        ] {
            assert!(said.contains(hint), "{hint} missing: {said}");
        }
    }
}

/// What the check guards against, in the parser haest's requests go
/// through: each refused URL dials a host other than the one meant, and
/// each passed one the host meant.
#[cfg(feature = "reqwest")]
#[test]
fn a_parser_dials_another_host_for_a_refused_url_and_the_meant_one_for_a_passed_one() {
    let host = |url: &str| reqwest::Url::parse(url).ok()?.host_str().map(String::from);
    let url = "http://user:1234/secret@broker.example/agents";
    let parsed = reqwest::Url::parse(url).expect("parses");
    assert_eq!(parsed.host_str(), Some("user"));
    assert_eq!(parsed.port(), Some(1234));
    assert_eq!(parsed.path(), "/secret@broker.example/agents");
    for url in &MISREAD[..6] {
        let meant = &url[url.rfind('@').expect("an @") + 1..];
        let meant = meant.split(['/', ':']).next();
        assert_ne!(host(url).as_deref(), meant, "{url}");
    }
    for (url, meant) in [
        (READ_RIGHT[0], "broker.example"),
        (READ_RIGHT[4], "host"),
        (READ_RIGHT[5], "broker.example"),
    ] {
        assert_eq!(host(url).as_deref(), Some(meant), "{url}");
    }
    // The parser drops the whitespace, then takes the path's first part
    // for the host; a URL with only a port or a userinfo dials nothing.
    for url in &NO_HOST_PART[..8] {
        assert_eq!(host(url), None, "{url:?}");
    }
    for url in &NO_HOST_PART[8..] {
        assert_eq!(host(url).as_deref(), Some("anthropic"), "{url:?}");
    }
    // A scheme with only slashes, a path put under it: the path's first
    // part is the host.
    for (joined, dialed) in [
        ("https:/v1/messages", "v1"),
        ("HTTP:\\\\/v1/messages", "v1"),
        ("https:\r/repos/o/r", "repos"),
        ("wss:/agents?x", "agents"),
    ] {
        assert_eq!(host(joined).as_deref(), Some(dialed), "{joined:?}");
    }
    let meant = ["localhost", "anthropic", "gw.example"];
    for (url, meant) in HOST_WITHOUT_SLASHES.into_iter().zip(meant) {
        assert_eq!(host(url).as_deref(), Some(meant), "{url:?}");
    }
}
