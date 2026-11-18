//! [`GithubTracker`]: GitHub Issues as a [`Tracker`](crate::Tracker).

use std::fmt;

use haest_http::{HttpClientRef, Method, redacted_url};
use serde_json::Value;

use crate::tracker::TrackerError;

mod api;
mod calls;
mod links;
mod wire;

use wire::{STATUSES, is_issue, is_pull_request};

const API: &str = "https://api.github.com";

/// The issues of one GitHub repository, as a [`Tracker`](crate::Tracker).
///
/// Over the REST API; deleting an issue is the one GraphQL call (REST has
/// no endpoint for it).
///
/// | Tracker | GitHub |
/// |---|---|
/// | id | the issue number (`42`; `owner/repo#42` for another repository's issue in a `child` or `depends_on` link) |
/// | status `open` / `closed` / `not_planned` | `state` + `state_reason` (`completed`, `not_planned`) |
/// | labels | labels (created on the fly by GitHub when new) |
/// | priority | none: GitHub issues have no priority field |
/// | parent / child | sub-issues |
/// | depends_on / blocks | issue dependencies ("blocked by" / "blocking") |
/// | relates_to | none |
///
/// The token is the host's: handed in with [`GithubTracker::with_token`], never
/// read from the environment. Without one, public repositories can be read.
/// Deleting an issue needs a token with admin rights on the repository.
///
/// PLUG POINT: [`GithubTracker::with_http`], how the requests travel (any
/// [`HttpClient`](haest_http::HttpClient); by default
/// [`HttpClientRef::default`], the shared `ReqwestClient` with the `reqwest`
/// feature).
///
/// Statuses are `open`, `closed` and `not_planned`; there are no priorities;
/// the relations are parent / child (sub-issues) and depends_on / blocks
/// (issue dependencies). An id is the issue number (`42` or `#42`), or
/// `owner/repo#42` for another repository's issue in a link. The issue a
/// call reads, changes, deletes or links is always this repository's: one
/// in another repository is refused as `Invalid`, unsent, whatever else the
/// token may reach. Only a link's other end may be elsewhere, and only for
/// a link GitHub keeps on this repository's issue (`child`, `depends_on`):
/// GitHub keeps a `parent` link on the parent and a `blocks` link on the
/// issue that waits, so one whose other end is elsewhere would change that
/// issue, and is refused the same way. So is a name GitHub cannot give an
/// owner or a repository (`..` among them). A new parent replaces an issue's
/// old one only when the old one is this repository's: replacing it takes
/// the link off the old parent, so an issue whose parent is elsewhere is
/// refused a new one. Pull requests are skipped everywhere: the issues API
/// lists them too, but they are not issues here.
///
/// GitHub's answers map to [`TrackerError`]: 404 and 410 are `NotFound`, 422
/// is `Invalid` with GitHub's reasons, anything else (a 401 without a token,
/// a 403, no answer within 60 s, an answer past 16 MiB) is `Failed`. A list
/// reads at most 10 pages.
///
/// # Examples
///
/// A fake [`HttpClient`](haest_http::HttpClient) answers in place of GitHub,
/// so this runs offline; a host leaves the default and hands in a token.
///
/// ```
/// use haest_issues::{
///     GithubTracker, IssueFilter, TrackerError,
///     http::{HttpClient, HttpError, Request, Response, async_trait},
/// };
/// use serde_json::json;
///
/// /// GitHub with one open issue.
/// struct OneIssue;
///
/// #[async_trait]
/// impl HttpClient for OneIssue {
///     async fn send(&self, req: Request) -> Result<Response, HttpError> {
///         assert!(req.url.starts_with("https://api.github.com/repos/owner/repo/issues?state=open"));
///         assert_eq!(req.header("authorization"), Some("Bearer a token"));
///         Ok(Response::new(200, "").with_json(&json!([{
///             "number": 7, "title": "Docs for haest-issues", "state": "open",
///             "repository_url": "https://api.github.com/repos/owner/repo",
///         }])))
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), TrackerError> {
/// let github = GithubTracker::new("owner", "repo")
///     .with_token("a token")
///     .with_http(OneIssue);
/// let open = github.list(&IssueFilter { limit: 10, ..Default::default() }).await?;
/// assert_eq!((open[0].id.as_str(), open[0].status.as_str()), ("7", "open"));
/// # Ok(())
/// # }
/// ```
///
/// Cloning is cheap enough to hand one to the tools and keep one: a clone
/// shares the client.
#[derive(Clone)]
pub struct GithubTracker {
    owner: String,
    repo: String,
    token: Option<String>,
    api: String,
    graphql: String,
    http: HttpClientRef,
}

/// Never the token; the API root without a `user:password@`.
impl fmt::Debug for GithubTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GithubTracker")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("token", &self.token.as_ref().map(|_| "<handed in>"))
            .field("api", &redacted_url(&self.api))
            .finish_non_exhaustive()
    }
}

impl GithubTracker {
    /// The issues of `owner/repo` on github.com, with no token, over the
    /// default client ([`HttpClientRef::default`]). Nothing is built or sent
    /// until the first call.
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            token: None,
            api: API.into(),
            graphql: format!("{API}/graphql"),
            http: HttpClientRef::default(),
        }
    }

    /// A token (a personal access token, an app's installation token): sent
    /// as `Authorization: Bearer`, nowhere else.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// The tracker sending its requests through `http`: a host's own client,
    /// a recorder, a test's fake (the client itself, or an `Arc` of one). The
    /// 60 s limit and the 16 MiB cap on an answer still hold.
    ///
    /// GitHub answers 301 for an issue moved to another repository. Give it
    /// a client that does not follow a 301, as the default does not: one that
    /// does repeats a change (`PATCH`, `DELETE`) at the issue's new
    /// repository. The tracker checks that an issue it reads before acting
    /// (a get, a delete, a label change, a link) is the one it asked for, and
    /// drops search results from elsewhere, so it never deletes or links
    /// another repository's issue, and a change answered from elsewhere is
    /// `Failed`; but a change it sent without reading the issue first has by
    /// then been made there.
    #[must_use]
    pub fn with_http(mut self, http: impl Into<HttpClientRef>) -> Self {
        self.http = http.into();
        self
    }

    /// Another API root: GitHub Enterprise Server's `https://HOST/api/v3`
    /// (its GraphQL endpoint is then `https://HOST/api/graphql`). The token,
    /// if any, goes to this host.
    ///
    /// A root with an `@` past its host, or with no host (`https://`, what
    /// `https://${GHE_HOST}` makes of an empty variable), is never sent to:
    /// every call fails (`Failed`) saying why (haest-http's
    /// `check_endpoint_url`). A password holding an unescaped `/`, `?` or
    /// `#` ends the host a URL parser reads there, so the token and the rest
    /// of the password would go to another host; it goes in percent-encoded
    /// (`/` as `%2F`). With no host, a parser takes the first part of the
    /// path for one.
    #[must_use]
    pub fn with_api_url(mut self, url: impl Into<String>) -> Self {
        let mut url = url.into();
        // Only the path's `/` go, never the `//` after the scheme: a root
        // with no host stays one, and the check refuses it.
        let host = url.find("://").map_or(0, |i| i + 3);
        url.truncate(url.trim_end_matches('/').len().max(host));
        // Enterprise Server's GraphQL is beside `/api/v3`: the `/v3` cut is
        // the path's, never a host named `v3`.
        self.graphql = match url.strip_suffix("/v3").filter(|root| root.len() >= host) {
            Some(root) => format!("{root}/graphql"),
            None => format!("{url}/graphql"),
        };
        self.api = url;
        self
    }

    /// The repository's owner.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The repository's name: the tracker works on `owner/repo`'s issues only.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The API root its requests go to ([`with_api_url`](Self::with_api_url);
    /// `https://api.github.com` by default), with no trailing `/` on its
    /// path.
    pub fn api_url(&self) -> &str {
        &self.api
    }

    /// The client its requests go through ([`with_http`](Self::with_http)).
    pub fn http(&self) -> &HttpClientRef {
        &self.http
    }

    /// Whether it sends a token ([`with_token`](Self::with_token)); the token
    /// itself is never read back.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn statuses() -> Vec<String> {
        STATUSES.iter().map(|s| s.to_string()).collect()
    }

    /// Issue `r` as GitHub has it, checked to be `r`: an answer that is
    /// another issue is NotFound, so nothing acts on it. A client that
    /// follows GitHub's 301 for an issue moved to another repository hands
    /// back that repository's issue.
    async fn read(&self, r: &api::IssueRef) -> Result<Value, TrackerError> {
        let missing = format!("there is no issue {} in {}/{}", r.number, r.owner, r.repo);
        let v = self
            .call(Method::Get, self.issue_url(r, ""), None, &missing)
            .await?;
        if !is_issue(&v, r) {
            return Err(TrackerError::NotFound(format!(
                "{missing}: GitHub answered with another issue (one moved elsewhere)"
            )));
        }
        Ok(v)
    }

    /// GitHub's issue `id` in full, or NotFound (a pull request is not an issue).
    async fn fetch(&self, id: &str) -> Result<(api::IssueRef, Value), TrackerError> {
        let r = self.own_ref(id)?;
        let v = self.read(&r).await?;
        if is_pull_request(&v) {
            return Err(TrackerError::NotFound(format!(
                "{} is a pull request, not an issue",
                self.label(&r)
            )));
        }
        Ok((r, v))
    }
}

#[cfg(test)]
mod tests;
