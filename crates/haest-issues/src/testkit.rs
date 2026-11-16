//! Fakes for this crate's tests: a GitHub that answers from a script, a tool
//! context, GitHub's issue JSON.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_core::{
    tool::{JsonMap, ToolContext},
    workspace::ShutWorkspace,
};
use haest_http::{HttpClient, HttpError, Request, Response};
use serde_json::{Value, json};

use crate::github::GithubTracker;

/// Answers each request with the next scripted reply and records it. Out of
/// script = an error, so an unexpected request fails loudly.
#[derive(Default)]
pub(crate) struct FakeHttp {
    replies: Mutex<VecDeque<Result<Response, HttpError>>>,
    calls: Mutex<Vec<Request>>,
    hang: bool,
}

impl FakeHttp {
    pub fn new(replies: impl IntoIterator<Item = Response>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
            ..Default::default()
        })
    }

    /// A network that never connects.
    pub fn down(why: &str) -> Arc<Self> {
        let refused = HttpError::Connect(why.into());
        Arc::new(Self {
            replies: Mutex::new([Err(refused)].into()),
            ..Default::default()
        })
    }

    /// A server that never answers.
    pub fn hanging() -> Arc<Self> {
        Arc::new(Self {
            hang: true,
            ..Default::default()
        })
    }

    pub fn calls(&self) -> Vec<Request> {
        self.calls.lock().unwrap().clone()
    }

    /// `METHOD url` of every request so far.
    pub fn sent(&self) -> Vec<String> {
        (self.calls().iter())
            .map(|c| format!("{} {}", c.method.as_str(), c.url))
            .collect()
    }
}

#[async_trait]
impl HttpClient for FakeHttp {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        self.calls.lock().unwrap().push(req);
        if self.hang {
            return std::future::pending().await;
        }
        let next = self.replies.lock().unwrap().pop_front();
        next.unwrap_or_else(|| Err(HttpError::Request("fake GitHub: out of replies".into())))
    }
}

pub(crate) const API: &str = "https://api.github.com";

/// The tracker for `o/r`, with a token, over `http`.
pub(crate) fn github(http: Arc<FakeHttp>) -> GithubTracker {
    GithubTracker::new("o", "r")
        .with_token("t0k3n")
        .with_http(http)
}

pub(crate) fn ok(body: Value) -> Response {
    Response::json(200, &body)
}

/// GitHub's JSON for issue `number` of o/r: open, no labels, no relations.
pub(crate) fn issue(number: u64, title: &str) -> Value {
    json!({
        "id": 1000 + number,
        "node_id": format!("I_node{number}"),
        "number": number,
        "title": title,
        "body": format!("About {title}."),
        "state": "open",
        "state_reason": null,
        "labels": [],
        "html_url": format!("https://github.com/o/r/issues/{number}"),
        "repository_url": format!("{API}/repos/o/r"),
        "created_at": "2026-11-01T10:00:00Z",
        "updated_at": "2026-11-02T10:00:00Z",
        "user": {"login": "author1"},
        "assignees": [],
        "comments": 0,
        "parent_issue_url": null,
        "sub_issues_summary": {"total": 0, "completed": 0, "percent_completed": 0},
        "issue_dependencies_summary": {"blocked_by": 0, "total_blocked_by": 0, "blocking": 0, "total_blocking": 0}
    })
}

/// `issue` with `key` set to `value`.
pub(crate) fn with(mut v: Value, key: &str, value: Value) -> Value {
    v[key] = value;
    v
}

/// A context for running a tool outside the loop.
pub(crate) fn ctx() -> ToolContext {
    ToolContext::new("/", Arc::new(ShutWorkspace))
}

pub(crate) fn input(v: Value) -> JsonMap {
    v.as_object().cloned().unwrap_or_default()
}
