//! What Debug output shows of a URL and of headers: never a credential. A
//! URL may carry `user:password@`, and a header a key (`Authorization`,
//! `x-api-key`, a proxy's own), so a request, a provider's options or a
//! tracker logged with `{:?}` must show neither.

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
