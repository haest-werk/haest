//! A GitHub issue's JSON as an [`Issue`], and a status as GitHub's
//! `state` + `state_reason`.

use serde_json::{Map, Value, json};

use super::{GithubTracker, api::IssueRef};
use crate::tracker::{Issue, Link, Relation};

/// Open; closed as done; closed as not planned.
pub(super) const STATUSES: [&str; 3] = ["open", "closed", "not_planned"];

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The status a GitHub issue is in.
pub(super) fn status_of(v: &Value) -> &'static str {
    match (
        v.get("state").and_then(Value::as_str),
        v.get("state_reason").and_then(Value::as_str),
    ) {
        (Some("open"), _) => "open",
        (_, Some("not_planned")) => "not_planned",
        _ => "closed",
    }
}

/// `status` as the fields a create or an update sends.
pub(super) fn state_fields(status: &str, into: &mut Map<String, Value>) {
    let (state, reason) = match status {
        "open" => ("open", None),
        "not_planned" => ("closed", Some("not_planned")),
        _ => ("closed", Some("completed")),
    };
    into.insert("state".into(), json!(state));
    if let Some(reason) = reason {
        into.insert("state_reason".into(), json!(reason));
    }
}

/// The issues endpoints answer for pull requests too; these are not issues here.
pub(super) fn is_pull_request(v: &Value) -> bool {
    v.get("pull_request").is_some_and(|p| !p.is_null())
}

pub(super) fn label_names(v: &Value) -> Vec<String> {
    let labels = v.get("labels").and_then(Value::as_array);
    labels
        .into_iter()
        .flatten()
        .filter_map(|l| match l {
            Value::String(s) => Some(s.clone()),
            other => text(other, "name"),
        })
        .collect()
}

/// `.../repos/{owner}/{repo}/issues/{number}` (an API or a web URL) as a reference.
pub(super) fn ref_from_url(url: &str) -> Option<IssueRef> {
    let parts: Vec<&str> = url.trim_end_matches('/').rsplit('/').take(4).collect();
    let [number, "issues", repo, owner] = parts[..] else {
        return None;
    };
    Some(IssueRef {
        owner: owner.into(),
        repo: repo.into(),
        number: number.parse().ok()?,
    })
}

/// The repository GitHub's issue JSON `v` says it is in, as
/// `(owner, repo)`, from its `repository_url`.
fn repo_of(v: &Value) -> Option<(&str, &str)> {
    (v.get("repository_url").and_then(Value::as_str))
        .and_then(|u| u.rsplit_once("/repos/"))
        .and_then(|(_, owner_repo)| owner_repo.split_once('/'))
}

/// Whether GitHub's issue JSON `v` is issue `r` (GitHub's names ignore
/// case). Not when a client that follows GitHub's 301 for an issue moved to
/// another repository handed back that repository's issue.
pub(super) fn is_issue(v: &Value, r: &IssueRef) -> bool {
    let number = v.get("number").and_then(Value::as_u64);
    number == Some(r.number)
        && repo_of(v).is_some_and(|(owner, repo)| {
            owner.eq_ignore_ascii_case(&r.owner) && repo.eq_ignore_ascii_case(&r.repo)
        })
}

impl GithubTracker {
    /// Whether GitHub's issue JSON `v` is this repository's.
    pub(super) fn is_home_issue(&self, v: &Value) -> bool {
        repo_of(v).is_some_and(|(owner, repo)| self.is_home_repo(owner, repo))
    }

    /// How the tools name the issue `v`, wherever it lives.
    pub(super) fn name_of(&self, v: &Value) -> String {
        let number = v.get("number").and_then(Value::as_u64).unwrap_or(0);
        match repo_of(v) {
            Some((owner, repo)) => self.label(&IssueRef {
                owner: owner.into(),
                repo: repo.into(),
                number,
            }),
            None => number.to_string(),
        }
    }

    /// GitHub's issue JSON as an [`Issue`]: its parent among its links (the one
    /// relation the issue itself names); a get adds the rest.
    pub(super) fn issue_of(&self, v: &Value) -> Issue {
        let mut extra = Vec::new();
        if let Some(author) = v.pointer("/user/login").and_then(Value::as_str) {
            extra.push(("author".into(), author.into()));
        }
        let assignees: Vec<&str> = (v.get("assignees").and_then(Value::as_array))
            .into_iter()
            .flatten()
            .filter_map(|a| a.get("login").and_then(Value::as_str))
            .collect();
        if !assignees.is_empty() {
            extra.push(("assignees".into(), assignees.join(", ")));
        }
        if let Some(m) = v.pointer("/milestone/title").and_then(Value::as_str) {
            extra.push(("milestone".into(), m.into()));
        }
        match v.get("comments").and_then(Value::as_u64) {
            Some(n) if n > 0 => extra.push(("comments".into(), n.to_string())),
            _ => {}
        }
        let parent = (v.get("parent_issue_url").and_then(Value::as_str))
            .and_then(ref_from_url)
            .map(|r| Link::new(Relation::Parent, self.label(&r)));
        Issue {
            id: self.name_of(v),
            title: text(v, "title").unwrap_or_default(),
            status: status_of(v).into(),
            priority: None,
            labels: label_names(v),
            body: text(v, "body").unwrap_or_default(),
            url: text(v, "html_url"),
            created: text(v, "created_at"),
            updated: text(v, "updated_at"),
            links: parent.into_iter().collect(),
            extra,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_and_reason_read_as_one_status_and_write_back_the_same() {
        let read = |state: &str, reason: Value| {
            status_of(&json!({"state": state, "state_reason": reason}))
        };
        assert_eq!(read("open", json!("reopened")), "open");
        assert_eq!(read("closed", json!("completed")), "closed");
        assert_eq!(read("closed", Value::Null), "closed");
        assert_eq!(read("closed", json!("duplicate")), "closed");
        assert_eq!(read("closed", json!("not_planned")), "not_planned");
        for status in STATUSES {
            let mut fields = Map::new();
            state_fields(status, &mut fields);
            assert_eq!(status_of(&Value::Object(fields)), status);
        }
    }

    #[test]
    fn an_issue_url_names_its_repository_and_number() {
        let api = ref_from_url("https://api.github.com/repos/o/r/issues/12").unwrap();
        assert_eq!(
            (api.owner.as_str(), api.repo.as_str(), api.number),
            ("o", "r", 12)
        );
        assert!(ref_from_url("https://api.github.com/repos/o/r/pulls/12").is_none());
        assert!(ref_from_url("https://api.github.com/repos/o/r/issues/x").is_none());
    }
}
