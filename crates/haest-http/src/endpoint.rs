//! The URLs haest is configured to dial: refused before anything connects
//! when a URL parser would dial another host than the one meant
//! ([`check_endpoint_url`]), and a path put under one ([`join_path`]).

use std::{borrow::Cow, fmt};

use crate::redact::{SCHEMES, after_scheme, shown_url};

/// `path` under `base`, a URL haest is configured with (a provider's base
/// URL, an Image API's): after `base`'s path, one `/` between however many
/// it ends in, and before its query and its fragment. A gateway's key or an
/// `api-version` in the query stays the query:
/// `https://gw.example/v1?key=k` and `chat/completions` make
/// `https://gw.example/v1/chat/completions?key=k`.
///
/// A URL [`check_endpoint_url`] refuses stays refused, and one it passes
/// stays passed: the path goes in after every `@` of a URL it passes, and
/// the `//` after the scheme stays. A scheme with only slashes after it
/// (`https:`) has no host to put the path under: the path goes after
/// `scheme://`, so the URL still has none (`https:///v1/messages`).
#[must_use]
pub fn join_path(base: &str, path: &str) -> String {
    let (base, after) = base.split_at(base.find(['?', '#']).unwrap_or(base.len()));
    if let Some(scheme) = bare_scheme(&as_parsed(base)) {
        return format!("{scheme}:///{path}{after}");
    }
    let (scheme, rest) = base.split_at(after_scheme(base));
    format!("{scheme}{}/{path}{after}", rest.trim_end_matches('/'))
}

/// What ends a URL's host for a parser: the first of these past the scheme
/// (a `\` only in an `http`, `https`, `ws` or `wss` URL, which reads it as a
/// `/`; it ends the host here in any URL).
const HOST_ENDS: [char; 4] = ['/', '?', '#', '\\'];

/// Where `url`'s authority (`user:password@host:port`) is, as
/// [`check_endpoint_url`] reads it: from past `scheme://` (`http`, `https`,
/// `ws` or `wss`; else from the start) to the first `/`, `?`, `#` or `\`
/// after that, or the end. In a URL the check passes, every `@` is in it, so
/// the last one ends the userinfo.
#[must_use]
pub fn authority(url: &str) -> std::ops::Range<usize> {
    let start = after_scheme(url);
    let end = url[start..]
        .find(HOST_ENDS)
        .map_or(url.len(), |i| start + i);
    start..end
}

/// Refuses `url`, a URL haest is configured to dial, when an `@` comes past
/// where a URL parser ends its host, or when it has no host: before
/// anything connects to it.
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
/// `@` as `%40`). A URL that needs a real `@` in its path, query or
/// fragment cannot be configured as it is: `%40` there makes another URL
/// (RFC 3986, section 2.2), which a server may or may not read the same.
///
/// An `@` inside the userinfo with none of those before it
/// (`https://u:p@ss@host/v1`) passes: a URL parser reads the host after the
/// last `@`. A client that writes its own `Host` header from the authority
/// may read it from after the first (tungstenite does), and send part of
/// the userinfo in that header: hand such a client the URL without its
/// userinfo, as haest's broker link does.
///
/// A URL with no host after `http://`, `https://`, `ws://` or `wss://`
/// (`https:///v1`, what `https://${HOST}/v1` makes of an empty variable) is
/// refused too: a parser skips the extra `/` and takes the first part of
/// the path for the host, so `https:///anthropic/v1` dials the host
/// `anthropic`. So is one whose authority holds only a userinfo or a port
/// (`wss://:8443/agents`, `https://user:pass@/v1`), which no dial reaches,
/// and a scheme with nothing after it but `/` or `\` (`https:`, `https:/`):
/// a parser skips them, so a path joined under it dials the path's first
/// part. A host after the scheme with no `//` (`http:localhost:8080`) is a
/// host, and passes.
///
/// The check reads the URL as a parser does: without the spaces and control
/// characters before and after it, and without any tab, CR or LF in it. A
/// CRLF `.env` that keeps the `\r` of an empty `HOST=` makes
/// `https://\r/anthropic`, which a parser reads as `https:///anthropic`.
///
/// For the URLs haest is configured with (a broker's, a provider's base
/// URL, an MCP server's, a tracker's API root), never a URL a tool fetches:
/// an `@` in a page's path (`https://registry.example/@scope/pkg`) is the
/// page's own.
///
/// # Errors
///
/// [`EndpointUrlError`] when an `@` comes after the first `/`, `?`, `#` or
/// `\` past the scheme, or no host comes between `scheme://` (and a
/// userinfo's last `@`) and the port or the first of them, or only `/` and
/// `\` come after `scheme:` up to its query, fragment or end. Its message
/// shows the URL as [`redacted_url`](crate::redacted_url) does, with its
/// control characters escaped (a CR as `\r`), and says how to write it.
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
/// // No host: a parser would dial the host `v1`.
/// assert!(check_endpoint_url("https:///v1").is_err());
/// // No host before the port: no dial reaches it.
/// assert!(check_endpoint_url("wss://:8443/agents").is_err());
/// // A scheme alone: a parser would dial the first part of a path under it.
/// assert!(check_endpoint_url("https:").is_err());
/// ```
pub fn check_endpoint_url(url: &str) -> Result<(), EndpointUrlError> {
    let seen = as_parsed(url);
    let host = authority(&seen);
    let why = match seen.rfind('@') {
        Some(at) if at >= host.end => Why::AtPastHost,
        _ if host.start > 0 && matches!(host_of(&seen[host]), "" | "[]") => Why::NoHost,
        _ if bare_scheme(&seen).is_some() => Why::NoHost,
        _ => return Ok(()),
    };
    Err(EndpointUrlError {
        shown: shown_url(url),
        why,
    })
}

/// `url` as a URL parser reads it (the WHATWG URL standard, which reqwest's
/// parser follows): without the control characters and spaces before and
/// after it, and without any tab, CR or LF in it.
pub fn as_parsed(url: &str) -> Cow<'_, str> {
    let url = url.trim_matches(|c: char| c <= ' ');
    if url.contains(['\t', '\r', '\n']) {
        Cow::Owned(url.replace(['\t', '\r', '\n'], ""))
    } else {
        Cow::Borrowed(url)
    }
}

/// The scheme of `seen`, a URL as a parser reads it, when it is `http`,
/// `https`, `ws` or `wss` with nothing after its `:` but `/` and `\` up to
/// its query, its fragment or its end (`https:`, `https:/`, `https://`): a
/// parser skips them and takes the first part of a path under it for the
/// host.
fn bare_scheme(seen: &str) -> Option<&str> {
    let (scheme, rest) = seen.split_once(':')?;
    let rest = rest.trim_start_matches(['/', '\\']);
    let bare = SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme))
        && (rest.is_empty() || rest.starts_with(['?', '#']));
    bare.then_some(scheme)
}

/// The host in `authority` (`user:password@host:port`): after its last `@`,
/// before its port. An IPv6 host keeps its brackets (`[::1]`, or `[]` for
/// none).
fn host_of(authority: &str) -> &str {
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    match host.find(']') {
        Some(end) if host.starts_with('[') => &host[..=end],
        _ => host.split(':').next().unwrap_or(host),
    }
}

/// A URL haest is configured to dial that [`check_endpoint_url`] refused: an
/// `@` comes past where a URL parser ends its host, so it may dial another
/// host than the one after that `@`; or it has no host, so a parser takes
/// the first part of its path for one, or dials nothing. Its message shows
/// the URL without its credentials ([`redacted_url`](crate::redacted_url))
/// and with its control characters escaped, and says how to write it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{shown} {why}")]
pub struct EndpointUrlError {
    shown: String,
    why: Why,
}

impl EndpointUrlError {
    /// The URL refused, as [`redacted_url`](crate::redacted_url) shows it,
    /// with each control character escaped (a CR as `\r`).
    pub fn url(&self) -> &str {
        &self.shown
    }
}

/// Why [`check_endpoint_url`] refused a URL: the rest of its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    AtPastHost,
    NoHost,
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AtPastHost => {
                "has an @ past its host, which haest cannot tell from one that ends a password: a URL parser ends the host at the first /, ?, # or \\, so a password holding one would send the rest of it, and the host after the @, to another host. Percent-encode the password (/ as %2F, ? as %3F, # as %23, \\ as %5C, @ as %40). A URL with an @ meant for its path, query or fragment cannot be used as it is: %40 there makes another URL, which the server may or may not read the same"
            }
            Self::NoHost => {
                "has no host: a URL parser would take the first part of its path for one, or dial nothing. Write the host after the // (and after a user:password@), and check that no variable it is made from is empty"
            }
        })
    }
}

#[cfg(test)]
mod tests;
