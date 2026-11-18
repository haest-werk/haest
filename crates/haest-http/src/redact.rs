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
pub(crate) const SCHEMES: [&str; 4] = ["http", "https", "ws", "wss"];

/// Where what follows `url`'s scheme starts: past `scheme://` for one of
/// [`SCHEMES`], else 0.
pub(crate) fn after_scheme(url: &str) -> usize {
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
/// credential shows. A password with an unescaped `/`, `?` or `#` (a pasted
/// base64 token) ends the authority a parser sees early, and an `@` in a
/// path cannot be told from one that ends such a password. An unescaped
/// `@` in the userinfo may sit on either side of where a reader ends it: a
/// URL parser takes the last `@`, tungstenite's `Host` header the first.
/// So everything between the scheme and the last `@` is hidden. When a `?` or
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

/// `url` as an error message shows it: [`redacted_url`], with each control
/// character escaped (a CR as `\r`), so a message printed to a terminal
/// cannot move the cursor or wipe the line it is on. (Debug output escapes
/// them already.)
#[must_use]
pub fn shown_url(url: &str) -> String {
    let shown = redacted_url(url);
    if !shown.chars().any(char::is_control) {
        return shown.into_owned();
    }
    let mut out = String::with_capacity(shown.len() + 8);
    for c in shown.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
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
mod tests;
