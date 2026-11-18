//! The [`Tracker`](crate::Tracker) calls on GitHub: REST requests, and the
//! one GraphQL mutation (`deleteIssue`) REST has no endpoint for.

use haest_http::Method;
use serde_json::{Map, Value, json};

use super::{
    GithubTracker,
    api::encode,
    wire::{is_issue, is_pull_request, label_names, state_fields, status_of},
};
use crate::tracker::{
    Issue, IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, TrackerError,
    TrackerInfo, forward_tracker, one_of,
};

/// The most pages a list reads looking for issues (pull requests are skipped).
const MAX_PAGES: u32 = 10;
const MAX_PER_PAGE: usize = 100;

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

/// The model's search text as plain words, so that GitHub reads none of it
/// as search syntax: `:` (a qualifier, `repo:` and `org:` among them), `"`
/// (a phrase) and parentheses (a group) split words, a leading `-` (NOT)
/// goes, and the operators AND, OR and NOT, in any case, are dropped. Words
/// GitHub ANDs with the tracker's own `repo:` only narrow the search.
fn search_words(query: &str) -> String {
    let syntax = |c: char| c.is_whitespace() || matches!(c, ':' | '"' | '(' | ')');
    let operator = |w: &str| {
        ["AND", "OR", "NOT"]
            .iter()
            .any(|op| w.eq_ignore_ascii_case(op))
    };
    let words: Vec<&str> = (query.split(syntax))
        .map(|w| w.trim_start_matches('-'))
        .filter(|w| !w.is_empty() && !operator(w))
        .collect();
    words.join(" ")
}

forward_tracker!(GithubTracker);

/// The [`Tracker`](crate::Tracker) calls, inherent: each needs no trait
/// import, and the trait's own forward here.
impl GithubTracker {
    /// What it is and accepts: the repository's issues, `open` and `closed`
    /// ([`Tracker::info`](crate::Tracker::info)).
    pub fn info(&self) -> TrackerInfo {
        TrackerInfo::new(
            format!("the GitHub issues of {}/{}", self.owner, self.repo),
            "the issue number, e.g. \"42\"",
            Self::statuses(),
        )
        .with_open_statuses(["open"])
        .with_relations([
            Relation::Parent,
            Relation::Child,
            Relation::DependsOn,
            Relation::Blocks,
        ])
    }

    /// The issues `filter` picks, newest first, pull requests skipped
    /// ([`Tracker::list`](crate::Tracker::list)).
    ///
    /// # Errors
    ///
    /// As [`TrackerError`] says: a priority or a status it does not have is
    /// `Invalid`, no such repository `NotFound`, GitHub's refusal `Failed`.
    pub async fn list(&self, filter: &IssueFilter) -> Result<Vec<Issue>, TrackerError> {
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

    /// GitHub's search for the words in `query` in this repository's issues,
    /// best first ([`Tracker::search`](crate::Tracker::search)). The words
    /// are only words: what GitHub would read as its search syntax (a
    /// qualifier's `:`, a phrase's `"`, a group's parentheses, a leading `-`,
    /// the operators AND, OR and NOT) is taken out, so nothing in them
    /// widens the search past this repository.
    ///
    /// # Errors
    ///
    /// `Failed` when GitHub refuses or does not answer.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Issue>, TrackerError> {
        let q = format!(
            "repo:{}/{} is:issue {}",
            self.owner,
            self.repo,
            search_words(query)
        );
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
            .filter(|v| !is_pull_request(v) && self.is_home_issue(v))
            .take(limit)
            .map(|v| self.issue_of(v))
            .collect())
    }

    /// Issue `id` of this repository in full, its links included
    /// ([`Tracker::get`](crate::Tracker::get)).
    ///
    /// # Errors
    ///
    /// `Invalid` for an id of another repository, `NotFound` for no such
    /// issue, `Failed` when GitHub refuses.
    pub async fn get(&self, id: &str) -> Result<Issue, TrackerError> {
        let (r, v) = self.fetch(id).await?;
        let mut issue = self.issue_of(&v);
        issue.links.extend(self.more_links(&r, &v).await?);
        Ok(issue)
    }

    /// A new issue in this repository, closed at once when `new.status` says
    /// so ([`Tracker::create`](crate::Tracker::create)).
    ///
    /// # Errors
    ///
    /// `Invalid` for a priority or a status it does not have, `Failed` when
    /// GitHub refuses.
    pub async fn create(&self, new: NewIssue) -> Result<Issue, TrackerError> {
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

    /// Changes issue `id` of this repository and returns it as it now is
    /// ([`Tracker::update`](crate::Tracker::update)).
    ///
    /// # Errors
    ///
    /// `Invalid` for an empty patch, a value it does not take or an id of
    /// another repository, `NotFound` for no such issue, `Failed` when GitHub
    /// refuses.
    pub async fn update(&self, id: &str, patch: IssuePatch) -> Result<Issue, TrackerError> {
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
        let r = self.own_ref(id)?;
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
        // A client that follows GitHub's 301 sent the change where the issue went.
        if !is_issue(&updated, &r) {
            return Err(TrackerError::Failed(format!(
                "GitHub answered the change to issue {} with another issue: the client \
                 followed a redirect, and the change may have gone there",
                r.number
            )));
        }
        Ok(self.issue_of(&updated))
    }

    /// Deletes issue `id` of this repository for good, as GitHub's
    /// `deleteIssue` does ([`Tracker::delete`](crate::Tracker::delete)).
    ///
    /// # Errors
    ///
    /// `Invalid` for an id of another repository, `NotFound` for no such
    /// issue, `Failed` when GitHub refuses (deleting needs admin rights).
    pub async fn delete(&self, id: &str) -> Result<(), TrackerError> {
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

    /// Adds or removes `link` on issue `id` of this repository
    /// ([`Tracker::link`](crate::Tracker::link)). The other end of a `child`
    /// or `depends_on` link may be in any repository; that of a `parent` or
    /// `blocks` link must be in this one, since GitHub keeps those on the
    /// other end.
    ///
    /// # Errors
    ///
    /// `Invalid` for an id of another repository, a `parent` or `blocks`
    /// link to one, a new parent for an issue whose parent is another
    /// repository's, a name GitHub cannot have, or a relation GitHub has no
    /// place for; `NotFound` for no such issue; `Failed` when GitHub refuses.
    pub async fn link(
        &self,
        id: &str,
        link: &Link,
        change: LinkChange,
    ) -> Result<(), TrackerError> {
        self.change_link(id, link, change).await
    }
}
