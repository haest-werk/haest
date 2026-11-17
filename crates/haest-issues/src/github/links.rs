//! Relations on GitHub: sub-issues (parent / child) and issue dependencies
//! (blocked by / blocking), both REST. GitHub has no "relates to" link.
//!
//! Both APIs take the OTHER issue by its database `id`, not its number, so a
//! change first reads that issue.

use haest_http::Method;
use serde_json::{Value, json};

use super::{GithubTracker, api::IssueRef};
use crate::tracker::{Link, LinkChange, Relation, TrackerError};

/// Children and dependencies past this many are not listed on a get.
const PER_PAGE: u32 = 100;

impl GithubTracker {
    /// The relations a get shows beyond the parent: children, what blocks the
    /// issue, what it blocks. Only asked for when the issue's own summary says
    /// there are any.
    pub(super) async fn more_links(
        &self,
        r: &IssueRef,
        raw: &Value,
    ) -> Result<Vec<Link>, TrackerError> {
        let count = |pointer: &str| raw.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
        let asks = [
            ("/sub_issues_summary/total", "/sub_issues", Relation::Child),
            (
                "/issue_dependencies_summary/total_blocked_by",
                "/dependencies/blocked_by",
                Relation::DependsOn,
            ),
            (
                "/issue_dependencies_summary/total_blocking",
                "/dependencies/blocking",
                Relation::Blocks,
            ),
        ];
        let mut links = Vec::new();
        for (summary, path, relation) in asks {
            if count(summary) == 0 {
                continue;
            }
            let url = self.issue_url(r, &format!("{path}?per_page={PER_PAGE}"));
            let listed = self
                .call(Method::Get, url, None, "the issue was not found")
                .await?;
            for item in listed.as_array().into_iter().flatten() {
                links.push(Link::new(relation, self.name_of(item)));
            }
        }
        Ok(links)
    }

    /// The database id GitHub's relation APIs take.
    async fn database_id(&self, r: &IssueRef) -> Result<u64, TrackerError> {
        let missing = format!("there is no issue {}", self.label(r));
        let v = self
            .call(Method::Get, self.issue_url(r, ""), None, &missing)
            .await?;
        v.get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| TrackerError::Failed("GitHub sent an issue with no id".into()))
    }

    pub(super) async fn change_link(
        &self,
        id: &str,
        link: &Link,
        change: LinkChange,
    ) -> Result<(), TrackerError> {
        let this = self.own_ref(id)?;
        let other = self.any_ref(&link.target)?;
        if this == other {
            return Err(TrackerError::Invalid(
                "an issue cannot be linked to itself".into(),
            ));
        }
        // Every relation as the one GitHub stores: (parent, child) or (blocked, blocker).
        let (holder, held, sub_issue) = match link.relation {
            Relation::Parent => (other, this, true),
            Relation::Child => (this, other, true),
            Relation::DependsOn => (this, other, false),
            Relation::Blocks => (other, this, false),
            Relation::RelatesTo => {
                return Err(TrackerError::Invalid(
                    "GitHub issues have no relates_to link; mention the other issue in the body instead".into(),
                ));
            }
        };
        let held_id = self.database_id(&held).await?;
        let missing = match change {
            LinkChange::Add => format!("there is no issue {}", self.label(&holder)),
            LinkChange::Remove => format!(
                "{} and {} are not linked that way",
                self.label(&holder),
                self.label(&held)
            ),
        };
        let (method, url, body) = match (sub_issue, change) {
            // An issue has one parent: a new one replaces it, as on a card.
            (true, LinkChange::Add) => (
                Method::Post,
                self.issue_url(&holder, "/sub_issues"),
                Some(json!({"sub_issue_id": held_id, "replace_parent": true})),
            ),
            (true, LinkChange::Remove) => (
                Method::Delete,
                self.issue_url(&holder, "/sub_issue"),
                Some(json!({"sub_issue_id": held_id})),
            ),
            (false, LinkChange::Add) => (
                Method::Post,
                self.issue_url(&holder, "/dependencies/blocked_by"),
                Some(json!({"issue_id": held_id})),
            ),
            (false, LinkChange::Remove) => (
                Method::Delete,
                self.issue_url(&holder, &format!("/dependencies/blocked_by/{held_id}")),
                None,
            ),
        };
        self.call(method, url, body, &missing).await.map(|_| ())
    }
}
