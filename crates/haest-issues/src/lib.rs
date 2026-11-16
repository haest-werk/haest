//! haest-issues: EXPERIMENTAL, and an example of how to write tools for haest.
//!
//! Issue-tracker tools for a haest agent, over one trait:
//!
//! - [`Tracker`] is the contract the tools talk to (list, search, get,
//!   create, update, delete, link).
//! - [`github::GithubTracker`]: the issues of one GitHub repository, over the
//!   REST API; the token is the host's to hand in.
//! - [`cards::CardTracker`]: board cards, one markdown file per card with its
//!   fields in YAML frontmatter, in a directory the host names.
//! - [`issue_tools`]: the seven haest tools (IssueList, IssueSearch, IssueGet,
//!   IssueCreate, IssueUpdate, IssueDelete, IssueLink) over any tracker.
//!
//! ```no_run
//! use std::sync::Arc;
//! use haest_issues::{issue_tools, github::GithubTracker};
//!
//! let tracker = GithubTracker::new("owner", "repo").with_token("a token the host holds");
//! let tools = issue_tools(Arc::new(tracker));
//! // ... put `tools` in the agent's AgentDeps, beside its other tools.
//! # let _ = tools;
//! ```
//!
//! What makes these haest tools, for anyone writing their own: each implements
//! [`haest_core::tool::Tool`] (a name, a description, a JSON Schema, a
//! canonical kind, `read_only`, `execute`); a call the model can fix comes back
//! as `ToolError::Input`, anything else as `ToolError::Failed`; a call gives up
//! when the turn is cancelled; and nothing is read that the host did not hand
//! in (no environment variable, no path of its own).

pub mod cards;
pub mod github;
#[cfg(test)]
mod testkit;
pub mod tools;
pub mod tracker;

pub use tools::{DELETE_KIND, issue_tools, issue_tools_named};
pub use tracker::{
    Issue, IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, Tracker, TrackerError,
    TrackerInfo,
};
