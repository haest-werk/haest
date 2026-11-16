//! The contract the issue tools talk to. A backend implements [`Tracker`]; the
//! tools ([`crate::issue_tools`]) never know which one they hold.
//!
//! PLUG POINT: implement [`Tracker`] for another tracker (Jira, Linear, a
//! database table) and every tool works on it unchanged.

use async_trait::async_trait;

/// One issue (a GitHub issue, a board card), as every backend reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Issue {
    /// What the tools and the model call it by: `42` on GitHub, the card's id.
    pub id: String,
    /// Its one-line title.
    pub title: String,
    /// One of the tracker's [`TrackerInfo::statuses`].
    pub status: String,
    /// One of the tracker's [`TrackerInfo::priorities`], when it has any.
    pub priority: Option<String>,
    /// GitHub's labels, a card's tags.
    pub labels: Vec<String>,
    /// Its description, markdown; empty when it has none.
    pub body: String,
    /// Where a human opens it: a web page, a file.
    pub url: Option<String>,
    /// When it was made, as the backend stores it (GitHub and a card haest
    /// made both write ISO 8601).
    pub created: Option<String>,
    /// When it last changed, when the backend knows (GitHub does, a card does not).
    pub updated: Option<String>,
    /// Its relations. A list or a search may leave them out; a get fills them in.
    pub links: Vec<Link>,
    /// Other facts the backend keeps (an author, a card's own keys), shown as `key: value`.
    pub extra: Vec<(String, String)>,
}

/// How one issue relates to another. Each relation has an inverse: `A parent B`
/// is `B child A`, `A depends_on B` is `B blocks A`; `relates_to` is its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Relation {
    /// The target is this issue's parent (a GitHub parent issue, a card's epic).
    Parent,
    /// The target is a child of this issue.
    Child,
    /// The target must be done first (GitHub "blocked by", a card's `depends_on`).
    DependsOn,
    /// The target waits on this issue.
    Blocks,
    /// The target is worth reading alongside this one; no order, no hierarchy.
    RelatesTo,
}

impl Relation {
    /// Every relation, in the order the tools list them.
    pub const ALL: [Relation; 5] = [
        Relation::Parent,
        Relation::Child,
        Relation::DependsOn,
        Relation::Blocks,
        Relation::RelatesTo,
    ];

    /// The word the tools use for it: `parent`, `child`, `depends_on`,
    /// `blocks`, `relates_to`.
    pub fn as_str(self) -> &'static str {
        match self {
            Relation::Parent => "parent",
            Relation::Child => "child",
            Relation::DependsOn => "depends_on",
            Relation::Blocks => "blocks",
            Relation::RelatesTo => "relates_to",
        }
    }

    /// The relation [`Relation::as_str`] spells `word`; None for any other word.
    pub fn parse(word: &str) -> Option<Relation> {
        Relation::ALL.into_iter().find(|r| r.as_str() == word)
    }

    /// The same fact seen from the target.
    pub fn inverse(self) -> Relation {
        match self {
            Relation::Parent => Relation::Child,
            Relation::Child => Relation::Parent,
            Relation::DependsOn => Relation::Blocks,
            Relation::Blocks => Relation::DependsOn,
            Relation::RelatesTo => Relation::RelatesTo,
        }
    }
}

/// One relation from the issue that holds the link: `<issue> <relation> <target>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// How the holder relates to the target.
    pub relation: Relation,
    /// The other issue's id, as the tracker names it.
    pub target: String,
}

impl Link {
    /// `relation` to the issue `target`.
    pub fn new(relation: Relation, target: impl Into<String>) -> Self {
        Self {
            relation,
            target: target.into(),
        }
    }
}

/// Whether [`Tracker::link`] adds a link or takes one off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkChange {
    /// Add the link (a parent replaces the one there).
    Add,
    /// Take the link off; NotFound when it is not there.
    Remove,
}

/// Which issues [`Tracker::list`] returns.
///
/// Set [`limit`](Self::limit): the default is 0, and a list of at most 0
/// issues is empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IssueFilter {
    /// Only these statuses; None = the tracker's [`TrackerInfo::open_statuses`].
    pub statuses: Option<Vec<String>>,
    /// Every one of these, on each issue returned.
    pub labels: Vec<String>,
    /// Only this priority; None = any. A tracker with no priorities refuses one.
    pub priority: Option<String>,
    /// At most this many issues.
    pub limit: usize,
}

/// A new issue for [`Tracker::create`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewIssue {
    /// Its title; a card refuses an empty one.
    pub title: String,
    /// Its description, markdown; may be empty.
    pub body: String,
    /// None = the tracker's first status.
    pub status: Option<String>,
    /// One of the tracker's priorities; None = none.
    pub priority: Option<String>,
    /// Its labels (tags on a card).
    pub labels: Vec<String>,
}

/// A change to one issue: None (or empty) leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IssuePatch {
    /// A new title.
    pub title: Option<String>,
    /// The whole new description.
    pub body: Option<String>,
    /// A new status, one of the tracker's.
    pub status: Option<String>,
    /// A new priority, one of the tracker's.
    pub priority: Option<String>,
    /// The whole new set of labels.
    pub labels: Option<Vec<String>>,
    /// Labels to add, after `labels` replaced the set.
    pub add_labels: Vec<String>,
    /// Labels to take off, after the adds.
    pub remove_labels: Vec<String>,
}

impl IssuePatch {
    /// Whether the patch changes nothing at all (a tracker refuses such an update).
    pub fn is_empty(&self) -> bool {
        self == &IssuePatch::default()
    }

    /// `current` labels with this patch applied, in their order, new ones last:
    /// replaced by `labels` when set, then `add_labels` added (once each),
    /// then `remove_labels` taken off.
    ///
    /// # Examples
    ///
    /// ```
    /// use haest_issues::IssuePatch;
    ///
    /// let patch = IssuePatch {
    ///     add_labels: vec!["urgent".into()],
    ///     remove_labels: vec!["triage".into()],
    ///     ..Default::default()
    /// };
    /// let current = ["bug".to_string(), "triage".to_string()];
    /// assert_eq!(patch.labels_from(&current), ["bug", "urgent"]);
    /// ```
    pub fn labels_from(&self, current: &[String]) -> Vec<String> {
        let mut out = self.labels.clone().unwrap_or_else(|| current.to_vec());
        for label in &self.add_labels {
            if !out.contains(label) {
                out.push(label.clone());
            }
        }
        out.retain(|l| !self.remove_labels.contains(l));
        out
    }

    /// Whether the patch touches labels at all.
    pub fn changes_labels(&self) -> bool {
        self.labels.is_some() || !self.add_labels.is_empty() || !self.remove_labels.is_empty()
    }
}

/// What a tracker is and accepts, for the tools' descriptions and schemas.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackerInfo {
    /// What the tools say they work on: "the GitHub issues of owner/repo".
    pub name: String,
    /// What an id looks like: "an issue number, e.g. \"42\"".
    pub id_hint: String,
    /// Every status, the one a new issue gets first.
    pub statuses: Vec<String>,
    /// The statuses a list shows when it names none: the work not finished.
    pub open_statuses: Vec<String>,
    /// Empty when the tracker has no priority.
    pub priorities: Vec<String>,
    /// The relations it stores; IssueLink offers only these.
    pub relations: Vec<Relation>,
}

/// Why a [`Tracker`] call failed. The tools hand `NotFound` and `Invalid` to
/// the model as a mistake it can fix, `Failed` as a failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrackerError {
    /// No issue by that id: the caller can correct it.
    #[error("{0}")]
    NotFound(String),
    /// A value the tracker refuses (an unknown status, a relation it does not
    /// have): the caller can correct it.
    #[error("{0}")]
    Invalid(String),
    /// Anything else: the network, the disk, permissions.
    #[error("{0}")]
    Failed(String),
}

/// Checks that `value` is one of `allowed`, for a backend validating a
/// status or a priority; `what` names it in the error ("status").
///
/// # Errors
///
/// [`TrackerError::Invalid`] naming the allowed values, or saying the tracker
/// has no `what` at all when `allowed` is empty.
pub fn one_of(value: &str, allowed: &[String], what: &str) -> Result<(), TrackerError> {
    if allowed.iter().any(|a| a == value) {
        return Ok(());
    }
    if allowed.is_empty() {
        return Err(TrackerError::Invalid(format!("this tracker has no {what}")));
    }
    Err(TrackerError::Invalid(format!(
        "\"{value}\" is not a {what} here; use one of: {}",
        allowed.join(", ")
    )))
}

/// An issue tracker: what the issue tools ([`crate::issue_tools`]) call.
///
/// Implementations are shared between tool calls (`Arc`), so a write may run
/// while another call reads. Every method's errors follow [`TrackerError`]: a
/// wrong id is `NotFound`, a value the tracker does not accept is `Invalid`,
/// anything else (the network, the disk) is `Failed`. A method takes no
/// cancel token: the tools give up on a call by dropping its future, so a
/// backend should leave nothing half-written when that happens.
#[async_trait]
pub trait Tracker: Send + Sync {
    /// What this tracker is and accepts; the tools' descriptions and schemas
    /// are written from it once, when they are built.
    fn info(&self) -> TrackerInfo;
    /// The issues `filter` picks, newest first, at most `filter.limit`.
    async fn list(&self, filter: &IssueFilter) -> Result<Vec<Issue>, TrackerError>;
    /// Issues matching `query` in any status, best first, at most `limit`.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Issue>, TrackerError>;
    /// One issue in full, its links included.
    async fn get(&self, id: &str) -> Result<Issue, TrackerError>;
    /// Makes a new issue and returns it, its id included.
    async fn create(&self, new: NewIssue) -> Result<Issue, TrackerError>;
    /// Changes issue `id` and returns it as it now is. An empty patch is `Invalid`.
    async fn update(&self, id: &str, patch: IssuePatch) -> Result<Issue, TrackerError>;
    /// Gone for good: no tracker here can bring it back.
    async fn delete(&self, id: &str) -> Result<(), TrackerError>;
    /// Add or remove `link` on issue `id`. An issue has at most one parent:
    /// adding another replaces it.
    async fn link(&self, id: &str, link: &Link, change: LinkChange) -> Result<(), TrackerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_relation_reads_back_and_inverts_twice_to_itself() {
        for r in Relation::ALL {
            assert_eq!(Relation::parse(r.as_str()), Some(r));
            assert_eq!(r.inverse().inverse(), r);
        }
        assert_eq!(Relation::parse("epic"), None);
    }

    #[test]
    fn a_label_patch_replaces_then_adds_then_removes() {
        let current = vec!["a".to_string(), "b".to_string()];
        let add = IssuePatch {
            add_labels: vec!["c".into(), "a".into()],
            remove_labels: vec!["b".into()],
            ..Default::default()
        };
        assert_eq!(add.labels_from(&current), ["a", "c"]);
        let replace = IssuePatch {
            labels: Some(vec!["x".into()]),
            add_labels: vec!["y".into()],
            ..Default::default()
        };
        assert_eq!(replace.labels_from(&current), ["x", "y"]);
        assert!(!IssuePatch::default().changes_labels());
        assert!(IssuePatch::default().is_empty());
    }

    #[test]
    fn a_value_outside_the_list_names_the_list() {
        let allowed = vec!["low".to_string(), "high".to_string()];
        assert!(one_of("low", &allowed, "priority").is_ok());
        let Err(TrackerError::Invalid(why)) = one_of("urgent", &allowed, "priority") else {
            panic!("an unknown value must be refused");
        };
        assert!(why.contains("low") && why.contains("high"));
        assert!(matches!(
            one_of("high", &[], "priority"),
            Err(TrackerError::Invalid(_))
        ));
    }
}
