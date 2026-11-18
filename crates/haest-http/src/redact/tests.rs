//! What a URL and headers show in Debug output and errors: never a
//! credential.

use super::*;

#[test]
fn userinfo_is_redacted_and_the_rest_of_the_url_kept() {
    let cases = [
        (
            "https://user:pa55@proxy.example.com:8443/v1",
            "https://<redacted>@proxy.example.com:8443/v1",
        ),
        ("http://token@host/", "http://<redacted>@host/"),
        // An `@` in the password: the last one ends the userinfo.
        ("https://u:p@ss@host/v1", "https://<redacted>@host/v1"),
        ("https://api.anthropic.com", "https://api.anthropic.com"),
        ("user:pw@host:1234/v1", "<redacted>@host:1234/v1"),
        ("", ""),
    ];
    for (url, shown) in cases {
        assert_eq!(redacted_url(url), shown, "{url}");
    }
}

#[test]
fn query_values_and_the_fragment_are_redacted_and_the_names_kept() {
    let cases = [
        (
            "https://u:p@h/mcp?api_key=k3y&v=2",
            "https://<redacted>@h/mcp?api_key=<redacted>&v=<redacted>",
        ),
        ("wss://h/a?s3cret", "wss://h/a?<redacted>"),
        (
            "https://h/a?x=1#t0ken",
            "https://h/a?x=<redacted>#<redacted>",
        ),
        ("https://h/a#access_token=t", "https://h/a#<redacted>"),
        (
            "https://h/a?a=1&&b=",
            "https://h/a?a=<redacted>&&b=<redacted>",
        ),
        ("https://h/a", "https://h/a"),
    ];
    for (url, shown) in cases {
        assert_eq!(redacted_url(url), shown, "{url}");
    }
}

/// A password with a `/`, `?` or `#` in it, unescaped (a pasted base64
/// token), ends the authority a parser sees early: a parser reads
/// `agent:abc` as a host and port, or fails on it. Whatever it makes of
/// the URL (an `@` in the password too), no byte of the userinfo shows.
#[test]
fn a_password_a_parser_misreads_is_hidden_whatever_it_reads() {
    let cases = [
        (
            "wss://agent:abc/def+ghi=@broker.example/agents",
            "wss://<redacted>@broker.example/agents",
        ),
        (
            "https://user:p/ss@mcp.example/mcp?key=k",
            "https://<redacted>@mcp.example/mcp?key=<redacted>",
        ),
        // `agent:2024` reads as a host and its port.
        (
            "wss://agent.v1:2024/k3y@broker.example/x",
            "wss://<redacted>@broker.example/x",
        ),
        // A token alone: `ab` reads as the host.
        ("https://ab/cd@host/x", "https://<redacted>@host/x"),
        ("https://u:p@s/s@host/x", "https://<redacted>@host/x"),
        // A `?` or `#` before the last `@`: what follows that `@` may be
        // a query value or the fragment, so nothing past the scheme shows.
        ("https://user:p?ss@mcp.example/mcp", "https://<redacted>"),
        ("https://user:p#ss@mcp.example/mcp", "https://<redacted>"),
        // No scheme, and a `://` in the password is not one.
        ("user:p/ss@host/x", "<redacted>@host/x"),
        ("user:a://b@host/x", "<redacted>@host/x"),
        ("ab://cd@host/x", "<redacted>@host/x"),
        ("user:pass@host", "<redacted>@host"),
        ("WSS://u:p@host/x", "WSS://<redacted>@host/x"),
    ];
    for (url, shown) in cases {
        assert_eq!(redacted_url(url), shown, "{url}");
    }
}

/// Each way a credential sits in a URL, with each character a parser
/// may take for the end of the userinfo: none of its pieces shows.
#[test]
fn no_piece_of_a_credential_shows_wherever_it_sits() {
    let pieces = ["K1x9", "Q7z2", "W3v4", "P5u6", "R8t0"];
    let passwords = [
        "K1x9/Q7z2",
        "K1x9?Q7z2",
        "K1x9#Q7z2",
        "K1x9@Q7z2",
        "K1x9:Q7z2",
        "K1x9/Q7z2?W3v4#P5u6@R8t0",
        "K1x9://Q7z2",
    ];
    let forms = [
        "wss://agent:{pw}@broker.example/x",
        "wss://{pw}@broker.example/x",
        "https://agent:{pw}@broker.example/x?v=1#f",
        "agent:{pw}@broker.example/x",
        "{pw}@broker.example",
    ];
    for pw in passwords {
        for form in forms {
            let url = form.replace("{pw}", pw);
            let shown = redacted_url(&url);
            for secret in pieces.iter().chain(&["agent"]) {
                assert!(!shown.contains(secret), "{secret} of {url}: {shown}");
            }
        }
    }
}

/// An `@` past the host cannot be told from a password's: everything up
/// to it is hidden. The URL shows less, never a credential.
#[test]
fn an_at_past_the_host_hides_what_comes_before_it() {
    let cases = [
        (
            "https://registry.example/@scope/pkg",
            "https://<redacted>@scope/pkg",
        ),
        ("http://[::1]:8080/@x", "http://<redacted>@x"),
        ("https://h/x?email=a@b.example&key=k", "https://<redacted>"),
    ];
    for (url, shown) in cases {
        assert_eq!(redacted_url(url), shown, "{url}");
    }
}

/// A URL in an error message shows its control characters escaped, its
/// credentials still hidden.
#[test]
fn a_shown_url_escapes_its_control_characters() {
    assert_eq!(shown_url("wss://u:p@h/x\r"), "wss://<redacted>@h/x\\r");
    assert_eq!(
        shown_url("https://\u{1b}[2K/v1\u{7}"),
        "https://\\u{1b}[2K/v1\\u{7}"
    );
    assert_eq!(shown_url("https://h/v1"), "https://h/v1");
}

#[test]
fn debug_shows_header_names_and_a_quoted_url() {
    let headers = [("Authorization".to_string(), "Bearer sk-1".to_string())];
    assert_eq!(
        format!("{:?}", HeaderNames(&headers)),
        r#"[("Authorization", "<redacted>")]"#
    );
    assert_eq!(
        format!("{:?}", UrlDebug("https://u:p@h/")),
        r#""https://<redacted>@h/""#
    );
}
