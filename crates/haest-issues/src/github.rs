//! GitHub Issues as a [`Tracker`]: the issues of one repository, over the REST
//! API. Deleting an issue is the one GraphQL call (REST has no endpoint for it).
//!
//! | Tracker | GitHub |
//! |---|---|
//! | id | the issue number (`42`; `owner/repo#42` for another repository's issue in a link) |
//! | status `open` / `closed` / `not_planned` | `state` + `state_reason` (`completed`, `not_planned`) |
//! | labels | labels (created on the fly by GitHub when new) |
//! | priority | none: GitHub issues have no priority field |
//! | parent / child | sub-issues |
//! | depends_on / blocks | issue dependencies ("blocked by" / "blocking") |
//! | relates_to | none |
//!
//! The token is the host's: handed in with [`GithubTracker::with_token`], never
//! read from the environment. Without one, public repositories can be read.
//! Deleting an issue needs a token with admin rights on the repository.
//!
//! PLUG POINT: [`http::Http`], how the requests travel ([`http::ReqwestHttp`]
//! by default).

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::tracker::{
    Issue, IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, Tracker, TrackerError,
    TrackerInfo, one_of,
};

mod api;
pub mod http;
mod links;
mod wire;

use api::encode;
use http::{Http, Method, ReqwestHttp};
use wire::{STATUSES, is_pull_request, label_names, state_fields, status_of};

const API: &str = "https://api.github.com";
/// The most pages a list reads looking for issues (pull requests are skipped).
const MAX_PAGES: u32 = 10;
const MAX_PER_PAGE: usize = 100;

/// The issues of one GitHub repository, as a [`Tracker`].
///
/// Statuses are `open`, `closed` and `not_planned`; there are no priorities;
/// the relations are parent / child (sub-issues) and depends_on / blocks
/// (issue dependencies). An id is the issue number (`42` or `#42`), or
/// `owner/repo#42` for another repository's issue in a link. Pull requests
/// are skipped everywhere: the issues API lists them too, but they are not
/// issues here.
///
/// GitHub's answers map to [`TrackerError`]: 404 and 410 are `NotFound`, 422
/// is `Invalid` with GitHub's reasons, anything else (a 401 without a token,
/// a 403, no answer) is `Failed`. A list reads at most 10 pages.
///
/// # Examples
///
/// A fake [`Http`] answers in place of GitHub, so this runs offline; a host
/// leaves the default and hands in a token.
///
/// ```
/// use std::sync::Arc;
///
/// use haest_issues::{
///     IssueFilter, Tracker,
///     github::{GithubTracker, http::{Http, HttpCall, HttpReply}},
/// };
/// use serde_json::json;
///
/// /// GitHub with one open issue.
/// struct OneIssue;
///
/// #[async_trait::async_trait]
/// impl Http for OneIssue {
///     async fn send(&self, call: HttpCall) -> Result<HttpReply, String> {
///         assert!(call.url.starts_with("https://api.github.com/repos/owner/repo/issues?state=open"));
///         assert_eq!(call.header("authorization"), Some("Bearer a token"));
///         Ok(HttpReply::json(200, &json!([{
///             "number": 7, "title": "Docs for haest-issues", "state": "open",
///             "repository_url": "https://api.github.com/repos/owner/repo",
///         }])))
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let github = GithubTracker::new("owner", "repo")
///     .with_token("a token")
///     .with_http(Arc::new(OneIssue));
/// let open = github.list(&IssueFilter { limit: 10, ..Default::default() }).await.unwrap();
/// assert_eq!((open[0].id.as_str(), open[0].status.as_str()), ("7", "open"));
/// # }
/// ```
pub struct GithubTracker {
    owner: String,
    repo: String,
    token: Option<String>,
    api: String,
    graphql: String,
    http: Arc<dyn Http>,
}

impl fmt::Debug for GithubTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GithubTracker")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("token", &self.token.as_ref().map(|_| "<handed in>"))
            .field("api", &self.api)
            .finish_non_exhaustive()
    }
}

impl GithubTracker {
    /// The issues of `owner/repo` on github.com, with no token, over
    /// [`ReqwestHttp`]. Nothing is sent until the first call.
    ///
    /// # Panics
    ///
    /// When reqwest cannot build its HTTP client ([`ReqwestHttp::new`]).
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            token: None,
            api: API.into(),
            graphql: format!("{API}/graphql"),
            http: Arc::new(ReqwestHttp::new()),
        }
    }

    /// A token (a personal access token, an app's installation token): sent
    /// as `Authorization: Bearer`, nowhere else.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// The tracker sending its requests through `http`: a host's own client,
    /// a recorder, a test's fake.
    pub fn with_http(mut self, http: Arc<dyn Http>) -> Self {
        self.http = http;
        self
    }

    /// Another API root: GitHub Enterprise Server's `https://HOST/api/v3`
    /// (its GraphQL endpoint is then `https://HOST/api/graphql`). The token,
    /// if any, goes to this host.
    pub fn with_api_url(mut self, url: &str) -> Self {
        let url = url.trim_end_matches('/');
        self.graphql = match url.strip_suffix("/v3") {
            Some(root) => format!("{root}/graphql"),
            None => format!("{url}/graphql"),
        };
        self.api = url.into();
        self
    }

    fn statuses() -> Vec<String> {
        STATUSES.iter().map(|s| s.to_string()).collect()
    }

    /// GitHub's issue `id` in full, or NotFound (a pull request is not an issue).
    async fn fetch(&self, id: &str) -> Result<(api::IssueRef, Value), TrackerError> {
        let r = self.issue_ref(id)?;
        let missing = format!("there is no issue {} in {}/{}", r.number, r.owner, r.repo);
        let v = self
            .call(Method::Get, self.issue_url(&r, ""), None, &missing)
            .await?;
        if is_pull_request(&v) {
            return Err(TrackerError::NotFound(format!(
                "{} is a pull request, not an issue",
                self.label(&r)
            )));
        }
        Ok((r, v))
    }
}

/// The `state` a list asks GitHub for, to cover every status in `wanted`.
fn state_for(wanted: &[String]) -> &'static str {
    let open = wanted.iter().any(|s| s == "open");
    let closed = wanted.iter().any(|s| s != "open");
    match (open, closed) {
        (true, false) => "open",
        (false, true) => "closed",
        _ => "all",
    }
}

#[async_trait]
impl Tracker for GithubTracker {
    fn info(&self) -> TrackerInfo {
        TrackerInfo {
            name: format!("the GitHub issues of {}/{}", self.owner, self.repo),
            id_hint: "the issue number, e.g. \"42\"".into(),
            statuses: Self::statuses(),
            open_statuses: vec!["open".into()],
            priorities: Vec::new(),
            relations: vec![
                Relation::Parent,
                Relation::Child,
                Relation::DependsOn,
                Relation::Blocks,
            ],
        }
    }

    async fn list(&self, filter: &IssueFilter) -> Result<Vec<Issue>, TrackerError> {
        if let Some(p) = &filter.priority {
            one_of(p, &[], "priority")?;
        }
        let wanted = filter
            .statuses
            .clone()
            .unwrap_or_else(|| vec!["open".into()]);
        for s in &wanted {
            one_of(s, &Self::statuses(), "status")?;
        }
        let per_page = filter.limit.clamp(1, MAX_PER_PAGE);
        let mut query = format!("?state={}&per_page={per_page}", state_for(&wanted));
        if !filter.labels.is_empty() {
            query.push_str(&format!("&labels={}", encode(&filter.labels.join(","))));
        }
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let url = self.repo_url(&format!("/issues{query}&page={page}"));
            let missing = format!("there is no repository {}/{}", self.owner, self.repo);
            let listed = self.call(Method::Get, url, None, &missing).await?;
            let items = listed.as_array().cloned().unwrap_or_default();
            for v in &items {
                if !is_pull_request(v) && wanted.iter().any(|s| s == status_of(v)) {
                    out.push(self.issue_of(v));
                }
            }
            if out.len() >= filter.limit || items.len() < per_page {
                break;
            }
        }
        out.truncate(filter.limit);
        Ok(out)
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Issue>, TrackerError> {
        let q = format!("repo:{}/{} is:issue {query}", self.owner, self.repo);
        let per_page = limit.clamp(1, MAX_PER_PAGE);
        let url = format!(
            "{}/search/issues?q={}&per_page={per_page}",
            self.api,
            encode(&q)
        );
        let found = self
            .call(Method::Get, url, None, "GitHub's search was not found")
            .await?;
        let items = found.get("items").and_then(Value::as_array);
        Ok(items
            .into_iter()
            .flatten()
            .filter(|v| !is_pull_request(v))
            .take(limit)
            .map(|v| self.issue_of(v))
            .collect())
    }

    async fn get(&self, id: &str) -> Result<Issue, TrackerError> {
        let (r, v) = self.fetch(id).await?;
        let mut issue = self.issue_of(&v);
        issue.links.extend(self.more_links(&r, &v).await?);
        Ok(issue)
    }

    async fn create(&self, new: NewIssue) -> Result<Issue, TrackerError> {
        if let Some(p) = &new.priority {
            one_of(p, &[], "priority")?;
        }
        if let Some(s) = &new.status {
            one_of(s, &Self::statuses(), "status")?;
        }
        let body = json!({"title": new.title, "body": new.body, "labels": new.labels});
        let missing = format!("there is no repository {}/{}", self.owner, self.repo);
        let created = self
            .call(Method::Post, self.repo_url("/issues"), Some(body), &missing)
            .await?;
        match new.status.as_deref() {
            None | Some("open") => Ok(self.issue_of(&created)),
            Some(status) => {
                let id = self.name_of(&created);
                let patch = IssuePatch {
                    status: Some(status.into()),
                    ..Default::default()
                };
                self.update(&id, patch).await
            }
        }
    }

    async fn update(&self, id: &str, patch: IssuePatch) -> Result<Issue, TrackerError> {
        if patch.is_empty() {
            return Err(TrackerError::Invalid("the update changes nothing".into()));
        }
        if let Some(p) = &patch.priority {
            one_of(p, &[], "priority")?;
        }
        let mut fields = Map::new();
        if let Some(status) = &patch.status {
            one_of(status, &Self::statuses(), "status")?;
            state_fields(status, &mut fields);
        }
        let r = self.issue_ref(id)?;
        if patch.changes_labels() {
            // Adding or removing works on the labels it has now.
            let current = match patch.labels {
                Some(_) => Vec::new(),
                None => label_names(&self.fetch(id).await?.1),
            };
            fields.insert("labels".into(), json!(patch.labels_from(&current)));
        }
        if let Some(title) = &patch.title {
            fields.insert("title".into(), json!(title));
        }
        if let Some(body) = &patch.body {
            fields.insert("body".into(), json!(body));
        }
        let missing = format!("there is no issue {} in {}/{}", r.number, r.owner, r.repo);
        let url = self.issue_url(&r, "");
        let updated = self
            .call(Method::Patch, url, Some(Value::Object(fields)), &missing)
            .await?;
        Ok(self.issue_of(&updated))
    }

    async fn delete(&self, id: &str) -> Result<(), TrackerError> {
        let (_, v) = self.fetch(id).await?;
        let node = v
            .get("node_id")
            .and_then(Value::as_str)
            .ok_or_else(|| TrackerError::Failed("GitHub sent an issue with no node_id".into()))?;
        let mutation =
            "mutation($id: ID!) { deleteIssue(input: {issueId: $id}) { clientMutationId } }";
        self.graphql(mutation, json!({"id": node}))
            .await
            .map(|_| ())
    }

    async fn link(&self, id: &str, link: &Link, change: LinkChange) -> Result<(), TrackerError> {
        self.change_link(id, link, change).await
    }
}

#[cfg(test)]
mod tests;
