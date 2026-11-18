//! The URLs haest is configured to dial that a URL parser misreads, refused
//! before anything connects ([`check_endpoint_url`]).

use crate::redact::{after_scheme, redacted_url};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
