//! The [`Tracker`] calls on GitHub: REST requests, and the one GraphQL
//! mutation (`deleteIssue`) REST has no endpoint for.

use async_trait::async_trait;
use haest_http::Method;
use serde_json::{Map, Value, json};

use super::{
    GithubTracker,
    api::encode,
    wire::{is_pull_request, label_names, state_fields, status_of},
};
use crate::tracker::{
    Issue, IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, Tracker, TrackerError,
    TrackerInfo, one_of,
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
