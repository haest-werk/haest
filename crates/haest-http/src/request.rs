//! One request, ready to send.

use std::{fmt, str::FromStr};

use bytes::Bytes;
use serde_json::Value;

use crate::redact::{HeaderNames, UrlDebug};

/// The HTTP methods haest sends. More may come in a minor release.
///
/// It reads and writes as the word on the wire, case-sensitive as HTTP is:
///
/// ```
/// use haest_http::Method;
///
/// # fn main() -> Result<(), haest_http::ParseMethodError> {
/// let method: Method = "PATCH".parse()?;
/// assert_eq!(method, Method::Patch);
/// assert_eq!(method.to_string(), "PATCH");
/// assert!("patch".parse::<Method>().is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Method {
    /// `GET`: read (a web page, a model list, an issue).
    Get,
    /// `POST`: a model call, an image request, a new issue.
    Post,
    /// `PATCH`: change part of something (an issue).
    Patch,
    /// `DELETE`: remove something (an issue's link).
    Delete,
}

impl Method {
    /// The method as it goes on the wire: `GET`, `POST`, `PATCH`, `DELETE`.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Method {
    type Err = ParseMethodError;

    /// The method `word` names, exactly as [`as_str`](Method::as_str) writes it.
    fn from_str(word: &str) -> Result<Self, Self::Err> {
        [Method::Get, Method::Post, Method::Patch, Method::Delete]
            .into_iter()
            .find(|m| m.as_str() == word)
            .ok_or_else(|| ParseMethodError {
                word: word.to_string(),
            })
    }
}

/// A word that names no [`Method`] haest sends: what `"PUT".parse::<Method>()`
/// fails with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("\"{word}\" is not an HTTP method haest sends (GET, POST, PATCH, DELETE)")]
pub struct ParseMethodError {
    word: String,
}

impl ParseMethodError {
    /// The word, as given.
    pub fn word(&self) -> &str {
        &self.word
    }
}

/// One request, ready to send: everything an [`HttpClient`](crate::HttpClient)
/// is handed.
///
/// Built with [`new`](Self::new), [`get`](Self::get) or [`post`](Self::post)
/// and the `with_*` setters. Its fields are there to read (a client takes
/// them apart by name, `let Request { method, url, .. } = req;`), and more
/// may come in a minor release, so it is never built by literal.
///
/// ```
/// use haest_http::{Method, Request};
/// use serde_json::json;
///
/// let req = Request::post("https://api.example.com/v1/items")
///     .with_header("Authorization", "Bearer sk-...")
///     .with_json(&json!({"name": "first"}));
/// assert_eq!(req.method, Method::Post);
/// assert_eq!(req.header("content-type"), Some("application/json"));
/// assert_eq!(req.json_body()["name"], "first");
///
/// // A client takes it apart by name.
/// let Request { url, headers, body, .. } = req;
/// assert_eq!(url, "https://api.example.com/v1/items");
/// assert_eq!((headers.len(), body.is_empty()), (2, false));
/// ```
///
/// Its Debug output shows no credential: the URL without its userinfo
/// (`user:password@`), query values or fragment, each header's name without
/// its value, and the body's length.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Request {
    /// The method.
    pub method: Method,
    /// The whole URL, query included, already encoded.
    pub url: String,
    /// Header names and values, in order; read one with [`header`](Self::header).
    pub headers: Vec<(String, String)>,
    /// The body; empty = none.
    pub body: Bytes,
}

impl Request {
    /// `method` on `url`, no headers, no body.
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Bytes::new(),
        }
    }

    /// A GET of `url`.
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }

    /// A POST to `url`, no body yet.
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::Post, url)
    }

    /// This request with `name: value`. A header of the same name, in any
    /// case, is replaced: a header sent twice reaches the server as two
    /// values, and one that reads the first never sees the second.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        set_header(&mut self.headers, name.into(), value.into());
        self
    }

    /// This request with `body` as its JSON text, and `content-type: application/json`.
    #[must_use]
    pub fn with_json(mut self, body: &Value) -> Self {
        self.body = Bytes::from(body.to_string());
        self.with_header("content-type", "application/json")
    }

    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// The body as JSON; Null when it is empty or not JSON (what a fake
    /// reads to check what was sent).
    pub fn json_body(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &UrlDebug(&self.url))
            .field("headers", &HeaderNames(&self.headers))
            .field("body", &format_args!("{} bytes", self.body.len()))
            .finish()
    }
}

/// `name: value` in `headers`, replacing a header of the same name in any
/// case: what `with_header` means on a request and on a response.
pub(crate) fn set_header(headers: &mut Vec<(String, String)>, name: String, value: String) {
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&name));
    headers.push((name, value));
}

/// `name`'s value in `headers`, matched case-insensitively.
pub(crate) fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_set_twice_is_sent_once_with_the_last_value() {
        let req = Request::get("https://a.test/")
            .with_header("Accept", "text/html")
            .with_header("accept", "application/json");
        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.header("ACCEPT"), Some("application/json"));
    }

    #[test]
    fn with_header_replaces_on_a_response_too() {
        let res = crate::Response::new(200, "")
            .with_header("Content-Type", "text/html")
            .with_json(&serde_json::json!({}));
        assert_eq!(res.headers.len(), 1);
        assert_eq!(res.header("content-type"), Some("application/json"));
    }

    #[test]
    fn a_body_that_is_not_json_reads_as_null() {
        assert_eq!(Request::get("https://a.test/").json_body(), Value::Null);
    }

    #[test]
    fn debug_shows_no_credential_of_the_url_or_the_headers() {
        let req = Request::post("https://user:pa55@gateway.test/v1/chat")
            .with_header("Authorization", "Bearer sk-secret")
            .with_json(&serde_json::json!({"model": "m"}));
        let shown = format!("{req:?}");
        for secret in ["pa55", "user:", "sk-secret"] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
        assert!(shown.contains("gateway.test/v1/chat"), "{shown}");
        assert!(shown.contains("Authorization"), "{shown}");

        let res = crate::Response::new(200, "").with_header("set-cookie", "session=abc123");
        let shown = format!("{res:?}");
        assert!(!shown.contains("abc123"), "{shown}");
        assert!(shown.contains("set-cookie"), "{shown}");
    }
}
