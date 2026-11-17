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

/// A URL as Debug output shows it: quoted, its userinfo
/// [redacted](without_userinfo).
pub struct UrlDebug<'a>(pub &'a str);

impl fmt::Debug for UrlDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*without_userinfo(self.0), f)
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
