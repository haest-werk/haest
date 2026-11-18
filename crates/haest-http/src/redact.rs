//! What Debug output shows of a URL and of headers: never a credential. A
//! URL may carry `user:password@`, and a header a key (`Authorization`,
//! `x-api-key`, a proxy's own), so a request, a provider's options or a
//! tracker logged with `{:?}` must show neither.
//!
//! And the URLs haest is configured to dial that a URL parser misreads,
//! refused before anything connects ([`check_endpoint_url`]).

use std::{borrow::Cow, fmt};

/// What a credential shows as in Debug output.
pub const REDACTED: &str = "<redacted>";

/// The schemes haest dials, which [`redacted_url`] shows. Any other text
/// before a `://` may be a credential's (`user:a://b@host`, a token
/// `ab://cd@host` with no scheme), and is hidden with it.
const SCHEMES: [&str; 4] = ["http", "https", "ws", "wss"];

/// Where what follows `url`'s scheme starts: past `scheme://` for one of
/// [`SCHEMES`], else 0.
fn after_scheme(url: &str) -> usize {
    match url.split_once("://") {
        Some((scheme, _)) if SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) => {
            scheme.len() + 3
        }
        _ => 0,
    }
}

/// `url` with nothing a credential may ride in, as haest shows a URL in its
/// Debug output and its errors: its userinfo (`user:password@`), each query
/// parameter's value (a key, a token) and a parameter with no name, and the
/// fragment, each shown as `<redacted>`. The scheme, host, path and the
/// query's names stay, so it still says where it goes. For a host that
/// shows a URL it was given (a broker's, an MCP server's).
///
/// It fails safe: whatever a parser makes of the URL, no byte of a
/// credential shows. A password with an unescaped `/`, `?`, `#` or `@` (a
/// pasted base64 token) ends the authority a parser sees early, and an `@`
/// in a path cannot be told from one that ends such a password, so
/// everything between the scheme and the last `@` is hidden. When a `?` or
/// a `#` comes before that `@`, what follows it may be a query value or the
/// fragment, and nothing past the scheme shows. The scheme shows only when
/// it is one haest dials (`http`, `https`, `ws`, `wss`): before an `@`, any
/// other text ending in `://` may be part of a credential, and is hidden
/// with it. A URL with an `@` past its host
/// (`https://registry.example/@scope/pkg`) shows less than it could, never
/// a credential.
///
/// ```
/// use haest_http::redacted_url;
///
/// let shown = redacted_url("wss://agent:s3cret@broker.example/agents?key=k3y#t");
/// assert_eq!(shown, "wss://<redacted>@broker.example/agents?key=<redacted>#<redacted>");
/// assert_eq!(redacted_url("https://api.example/v1"), "https://api.example/v1");
/// // A `/` in the password: a parser reads `agent:ab` as the host and a port.
/// let shown = redacted_url("wss://agent:ab/cd@broker.example/agents");
/// assert_eq!(shown, "wss://<redacted>@broker.example/agents");
/// assert_eq!(redacted_url("https://user:p?ss@mcp.example/mcp"), "https://<redacted>");
/// ```
pub fn redacted_url(url: &str) -> Cow<'_, str> {
    let (scheme, rest) = url.split_at(after_scheme(url));
    let (hidden, rest) = match rest.rfind('@') {
        None => (false, rest),
        Some(at) if rest[..at].contains(['?', '#']) => {
            return Cow::Owned(format!("{scheme}{REDACTED}"));
        }
        // From the `@` on: the first `?` or `#` past it starts the query or
        // the fragment however the userinfo is read.
        Some(at) => (true, &rest[at..]),
    };
    let Some(cut) = rest.find(['?', '#']) else {
        return if hidden {
            Cow::Owned(format!("{scheme}{REDACTED}{rest}"))
        } else {
            Cow::Borrowed(url)
        };
    };
    let (base, rest) = rest.split_at(cut);
    let (query, fragment) = match rest.strip_prefix('?') {
        Some(after) => match after.split_once('#') {
            Some((query, _)) => (Some(query), true),
            None => (Some(after), false),
        },
        None => (None, true),
    };
    let mut shown = scheme.to_string();
    if hidden {
        shown.push_str(REDACTED);
    }
    shown.push_str(base);
    if let Some(query) = query {
        shown.push('?');
        for (n, pair) in query.split('&').enumerate() {
            if n > 0 {
                shown.push('&');
            }
            match pair.split_once('=') {
                Some((name, _)) => {
                    shown.push_str(name);
                    shown.push('=');
                    shown.push_str(REDACTED);
                }
                None if pair.is_empty() => {}
                None => shown.push_str(REDACTED),
            }
        }
    }
    if fragment {
        shown.push('#');
        shown.push_str(REDACTED);
    }
    Cow::Owned(shown)
}

/// What ends a URL's host for a parser: the first of these past the scheme
/// (a `\` only in an `http`, `https`, `ws` or `wss` URL, which reads it as a
/// `/`; it ends the host here in any URL).
const HOST_ENDS: [char; 4] = ['/', '?', '#', '\\'];

/// Refuses `url`, a URL haest is configured to dial, when an `@` comes past
/// where a URL parser ends its host: before anything connects to it.
///
/// A parser ends the host at the first `/`, `?` or `#` after the scheme
/// (and at a `\` in an `http`, `https`, `ws` or `wss` URL). A password that
/// holds one unescaped (a pasted base64 token) ends it early:
/// `http://user:1234/secret@broker.example/agents` dials the host `user`
/// on port 1234, and sends `/secret@broker.example/agents` (the rest of the
/// password and the host meant) as the path, in the clear, to that host. An
/// `@` in a path, a query or a fragment cannot be told from one that ends
/// such a password, so every one is refused. Write the password
/// percent-encoded (`/` as `%2F`, `?` as `%3F`, `#` as `%23`, `\` as `%5C`,
/// `@` as `%40`), and an `@` meant for the path, the query or the fragment
/// as `%40`. An `@` inside the userinfo with none of those before it
/// (`https://u:p@ss@host/v1`) is read the same by every parser, and passes.
///
/// For the URLs haest is configured with (a broker's, a provider's base
/// URL, an MCP server's, a tracker's API root), never a URL a tool fetches:
/// an `@` in a page's path (`https://registry.example/@scope/pkg`) is the
/// page's own.
///
/// # Errors
///
/// [`EndpointUrlError`] when an `@` comes after the first `/`, `?`, `#` or
/// `\` past the scheme. Its message shows the URL as [`redacted_url`] does,
/// and says how to write it.
///
/// ```
/// use haest_http::check_endpoint_url;
///
/// // A `/` in the password: a parser would dial the host `user`, port 1234.
/// let refused = check_endpoint_url("http://user:1234/secret@broker.example/agents");
/// assert!(refused.is_err_and(|e| !e.to_string().contains("secret")));
/// // The same password, percent-encoded.
/// assert!(check_endpoint_url("http://user:1234%2Fsecret@broker.example/agents").is_ok());
/// assert!(check_endpoint_url("https://api.example.com/v1").is_ok());
/// ```
pub fn check_endpoint_url(url: &str) -> Result<(), EndpointUrlError> {
    let rest = &url[after_scheme(url)..];
    match rest.rfind('@') {
        Some(at) if rest[..at].contains(HOST_ENDS) => Err(EndpointUrlError {
            shown: redacted_url(url).into_owned(),
        }),
        _ => Ok(()),
    }
}

/// A URL haest is configured to dial that [`check_endpoint_url`] refused: an
/// `@` comes past where a URL parser ends its host, so it would dial another
/// host than the one after that `@`. Its message shows the URL without its
/// credentials ([`redacted_url`]), and says how to write it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{shown} has an @ past its host, which a URL parser misreads: it ends the host at the first /, ?, # or \\, so a password holding one would send the rest of it, and the host after the @, to another host. Percent-encode the password (/ as %2F, ? as %3F, # as %23, \\ as %5C, @ as %40), and an @ meant for the path, the query or the fragment as %40"
)]
pub struct EndpointUrlError {
    shown: String,
}

impl EndpointUrlError {
    /// The URL refused, as [`redacted_url`] shows it.
    pub fn url(&self) -> &str {
        &self.shown
    }
}

/// A URL as Debug output shows it: quoted, with nothing a credential may
/// ride in ([`redacted_url`]).
pub struct UrlDebug<'a>(pub &'a str);

impl fmt::Debug for UrlDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*redacted_url(self.0), f)
    }
}

/// Headers as Debug output shows them: each name, never its value.
pub struct HeaderNames<'a>(pub &'a [(String, String)]);

impl fmt::Debug for HeaderNames<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self.0.iter().map(|(name, _)| (name, REDACTED));
        f.debug_list().entries(names).finish()
    }
}

#[cfg(test)]
mod tests {
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

    /// A password with a `/`, `?`, `#` or `@` in it, unescaped (a pasted
    /// base64 token), ends the authority a parser sees early: a parser reads
    /// `agent:abc` as a host and port, or fails on it. Whatever it makes of
    /// the URL, no byte of the userinfo shows.
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

    /// Passed: every parser reads the host after the last `@` of the
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
            for hint in ["%2F", "%3F", "%23", "%5C", "%40", "a URL parser misreads"] {
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
}
