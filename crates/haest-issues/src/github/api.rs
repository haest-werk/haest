//! Talking to GitHub: issue references, URLs, headers, and what a refusal means.

use std::time::Duration;

use haest_http::{Bytes, HttpError, Method, Request};
use serde_json::Value;

use super::GithubTracker;
use crate::tracker::TrackerError;

/// GitHub's REST API version this backend was written against.
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "haest-issues";
/// The most of an answer that is read; a list of 100 issues is well under it.
const MAX_BODY: usize = 16 * 1024 * 1024;
/// The longest one request may take, answer included.
const TIMEOUT: Duration = Duration::from_secs(60);

/// One issue: in the tracker's own repository, or another (`owner/repo#5`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IssueRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl GithubTracker {
    /// `42`, `#42` or `owner/repo#42`.
    pub(super) fn issue_ref(&self, id: &str) -> Result<IssueRef, TrackerError> {
        let id = id.trim();
        let (repo, number) = match id.rsplit_once('#') {
            Some((repo, n)) if !repo.is_empty() => (Some(repo), n),
            Some((_, n)) => (None, n),
            None => (None, id),
        };
        let bad = || {
            TrackerError::Invalid(format!(
                "\"{id}\" is not an issue: give its number, e.g. 42 (or owner/repo#42)"
            ))
        };
        let number: u64 = number.parse().map_err(|_| bad())?;
        let (owner, repo) = match repo {
            None => (self.owner.clone(), self.repo.clone()),
            Some(full) => {
                let (o, r) = full.split_once('/').ok_or_else(bad)?;
                if o.is_empty() || r.is_empty() || r.contains('/') {
                    return Err(bad());
                }
                (o.to_string(), r.to_string())
            }
        };
        Ok(IssueRef {
            owner,
            repo,
            number,
        })
    }

    /// How the tools name `r`: its number at home, `owner/repo#n` elsewhere.
    pub(super) fn label(&self, r: &IssueRef) -> String {
        if r.owner.eq_ignore_ascii_case(&self.owner) && r.repo.eq_ignore_ascii_case(&self.repo) {
            r.number.to_string()
        } else {
            format!("{}/{}#{}", r.owner, r.repo, r.number)
        }
    }

    /// `{api}/repos/{owner}/{repo}{rest}`, for this tracker's repository.
    pub(super) fn repo_url(&self, rest: &str) -> String {
        format!(
            "{}/repos/{}/{}{rest}",
            self.api,
            encode(&self.owner),
            encode(&self.repo)
        )
    }

    /// The issue's own URL, `rest` appended.
    pub(super) fn issue_url(&self, r: &IssueRef, rest: &str) -> String {
        format!(
            "{}/repos/{}/{}/issues/{}{rest}",
            self.api,
            encode(&r.owner),
            encode(&r.repo),
            r.number
        )
    }

    /// `url`, with GitHub's headers (the token, when there is one) and `body` as JSON.
    fn request(&self, method: Method, url: String, body: Option<&Value>) -> Request {
        let mut req = Request::new(method, url)
            .with_header("accept", "application/vnd.github+json")
            .with_header("x-github-api-version", API_VERSION)
            .with_header("user-agent", USER_AGENT);
        if let Some(token) = &self.token {
            req = req.with_header("authorization", format!("Bearer {token}"));
        }
        match body {
            Some(body) => req.with_json(body),
            None => req,
        }
    }

    /// Send `req` and read the whole answer, within [`TIMEOUT`] and [`MAX_BODY`].
    async fn exchange(&self, req: Request) -> Result<(u16, Bytes), HttpError> {
        let exchange = async {
            let res = self.http.send(req).await?;
            Ok((res.status, res.body.bytes(MAX_BODY).await?))
        };
        tokio::time::timeout(TIMEOUT, exchange)
            .await
            .unwrap_or(Err(HttpError::Timeout(TIMEOUT)))
    }

    /// One request; the answer's JSON (Null for an empty body). A 404 or 410
    /// becomes `NotFound(missing)`.
    pub(super) async fn call(
        &self,
        method: Method,
        url: String,
        body: Option<Value>,
        missing: &str,
    ) -> Result<Value, TrackerError> {
        let (status, answer) = self
            .exchange(self.request(method, url, body.as_ref()))
            .await
            .map_err(|e| TrackerError::Failed(format!("GitHub did not answer: {}", e.report())))?;
        let json: Value = if answer.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&answer).unwrap_or(Value::Null)
        };
        if (200..300).contains(&status) {
            return Ok(json);
        }
        let why = refusal(&json);
        Err(match status {
            404 | 410 => TrackerError::NotFound(missing.to_string()),
            422 => TrackerError::Invalid(format!("GitHub refused the change: {why}")),
            401 if self.token.is_none() => TrackerError::Failed(format!(
                "GitHub wants a token for this (401: {why}); the host gives the tracker one"
            )),
            status => TrackerError::Failed(format!("GitHub answered {status}: {why}")),
        })
    }

    /// One GraphQL operation (only what REST has no endpoint for); its `data`.
    pub(super) async fn graphql(
        &self,
        query: &str,
        variables: Value,
    ) -> Result<Value, TrackerError> {
        let body = serde_json::json!({"query": query, "variables": variables});
        let answer = self
            .call(
                Method::Post,
                self.graphql.clone(),
                Some(body),
                "GitHub's GraphQL endpoint was not found",
            )
            .await?;
        match answer.get("errors").and_then(Value::as_array) {
            Some(errors) if !errors.is_empty() => {
                let said: Vec<&str> = errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(Value::as_str))
                    .collect();
                Err(TrackerError::Failed(format!(
                    "GitHub refused: {}",
                    said.join("; ")
                )))
            }
            _ => Ok(answer.get("data").cloned().unwrap_or(Value::Null)),
        }
    }
}

/// GitHub's own words: `message`, then each of `errors` (a string, or an
/// object with a `message` or a `field` and a `code`).
fn refusal(json: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = json.get("message").and_then(Value::as_str) {
        parts.push(m.to_string());
    }
    for e in json
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let text = match e {
            Value::String(s) => s.clone(),
            other => match other.get("message").and_then(Value::as_str) {
                Some(m) => m.to_string(),
                None => format!(
                    "{} {}",
                    other.get("field").and_then(Value::as_str).unwrap_or("?"),
                    other
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("invalid")
                ),
            },
        };
        parts.push(text);
    }
    if parts.is_empty() {
        "no reason given".into()
    } else {
        parts.join("; ")
    }
}

/// Percent-encoding for a path segment or a query value: RFC 3986's unreserved
/// characters stay, every other byte becomes `%XX`.
pub(super) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_keeps_unreserved_bytes_only() {
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode("is:issue bug"), "is%3Aissue%20bug");
        assert_eq!(encode("é/#&"), "%C3%A9%2F%23%26");
    }

    #[test]
    fn a_refusal_reads_githubs_message_and_each_error() {
        let body = serde_json::json!({
            "message": "Validation Failed",
            "errors": [{"resource": "Issue", "field": "title", "code": "missing_field"}, "plain"]
        });
        assert_eq!(
            refusal(&body),
            "Validation Failed; title missing_field; plain"
        );
        assert_eq!(refusal(&Value::Null), "no reason given");
    }
}
