//! What Debug output shows of a URL and of headers: never a credential. A
//! URL may carry `user:password@`, and a header a key (`Authorization`,
//! `x-api-key`, a proxy's own), so a request, a provider's options or a
//! tracker logged with `{:?}` must show neither.

use std::{borrow::Cow, fmt};

/// What a credential shows as in Debug output.
pub const REDACTED: &str = "<redacted>";

/// `url` with its userinfo (`user:password@`), if any, shown as
/// `<redacted>@`; the scheme, host, path and query as they are.
pub fn without_userinfo(url: &str) -> Cow<'_, str> {
    let start = url.find("://").map_or(0, |i| i + 3);
    let rest = &url[start..];
    let authority = rest.find(['/', '?', '#']).map_or(rest, |end| &rest[..end]);
    match authority.rfind('@') {
        Some(at) => Cow::Owned(format!("{}{REDACTED}{}", &url[..start], &rest[at..])),
        None => Cow::Borrowed(url),
    }
}

/// `url` with nothing a credential may ride in: its userinfo
/// [redacted](without_userinfo), each query parameter's value (a key, a
/// token: `?api_key=<redacted>`) and a parameter with no name, and the
/// fragment. The scheme, host, path and the query's names as they are.
pub fn redacted_url(url: &str) -> Cow<'_, str> {
    let url = without_userinfo(url);
    let Some(cut) = url.find(['?', '#']) else {
        return url;
    };
    let (base, rest) = url.split_at(cut);
    let (query, fragment) = match rest.strip_prefix('?') {
        Some(after) => match after.split_once('#') {
            Some((query, _)) => (Some(query), true),
            None => (Some(after), false),
        },
        None => (None, true),
    };
    let mut shown = base.to_string();
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
                "https://user:pa55@proxy.example.com:8443/v1?x=1",
                "https://<redacted>@proxy.example.com:8443/v1?x=1",
            ),
            ("http://token@host/", "http://<redacted>@host/"),
            // An `@` in the password: the last one ends the userinfo.
            ("https://u:p@ss@host/v1", "https://<redacted>@host/v1"),
            // An `@` past the host is a path's or a query's, not userinfo.
            ("https://host/@me?to=a@b", "https://host/@me?to=a@b"),
            ("https://api.anthropic.com", "https://api.anthropic.com"),
            ("user:pw@host:1234/v1", "<redacted>@host:1234/v1"),
            ("", ""),
        ];
        for (url, shown) in cases {
            assert_eq!(without_userinfo(url), shown, "{url}");
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
