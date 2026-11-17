//! haest-issues: EXPERIMENTAL, and an example of how to write tools for haest.
//!
//! The API may change in any release, and the crate is not part of the
//! `haest` facade: depend on it directly. It is meant to be read as much as
//! used -- a small, complete family of haest tools over one backend trait.
//!
//! What it does: issue-tracker tools for a haest agent, over one trait.
//!
//! - [`Tracker`] is the contract the tools talk to (list, search, get,
//!   create, update, delete, link). Implement it for another tracker and every
//!   tool works on it unchanged.
//! - [`GithubTracker`]: the issues of one GitHub repository, over the
//!   REST API; the token is the host's to hand in.
//! - [`CardTracker`]: board cards, one markdown file per card with its
//!   fields in YAML frontmatter, in a directory the host names.
//! - [`issue_tools`]: the seven haest tools (IssueList, IssueSearch, IssueGet,
//!   IssueCreate, IssueUpdate, IssueDelete, IssueLink) over any tracker;
//!   [`issue_tools_named`] when one agent has two trackers.
//!
//! # Examples
//!
//! A board in memory, and the tools a model would call on it. In a host, the
//! tools go in the agent's `AgentDeps` beside its others, and the model calls
//! them; here they are called directly.
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_core::{MemFs, ShutWorkspace, ToolContext};
//! use haest_issues::{CardTracker, Tracker, issue_tools};
//! use serde_json::json;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! // The host names the file system and the directory; the tracker reads nothing else.
//! let board = Arc::new(CardTracker::new(Arc::new(MemFs::new()), "/project/cards"));
//! let tools = issue_tools(board.clone());
//! let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
//! assert_eq!(names, ["IssueList", "IssueSearch", "IssueGet", "IssueCreate", "IssueUpdate", "IssueDelete", "IssueLink"]);
//!
//! let ctx = ToolContext::new("/project", Arc::new(ShutWorkspace));
//! let call = |name: &str, input: serde_json::Value| {
//!     let tool = tools.iter().find(|t| t.name() == name).unwrap().clone();
//!     let (input, ctx) = (input.as_object().cloned().unwrap(), ctx.clone());
//!     async move { tool.execute(input, &ctx).await }
//! };
//!
//! call("IssueCreate", json!({"title": "Fix the login", "priority": "high"})).await.unwrap();
//! // The card's id comes from its title; a new card starts in the inbox.
//! let card = board.get("fix-the-login").await.unwrap();
//! assert_eq!((card.status.as_str(), card.priority.as_deref()), ("inbox", Some("high")));
//!
//! let listed = call("IssueList", json!({})).await.unwrap();
//! assert!(listed.content.contains("fix-the-login"));
//!
//! // A mistake the model can fix comes back as an input error it reads.
//! assert!(call("IssueGet", json!({"id": "no-such-card"})).await.is_err());
//! # }
//! ```
//!
//! GitHub is the same tools over another tracker. Building one sends nothing;
//! the first tool call does.
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_issues::{GithubTracker, issue_tools_named};
//!
//! let github = GithubTracker::new("owner", "repo").with_token("a token the host holds");
//! let tools = issue_tools_named(Arc::new(github), "Github");
//! assert_eq!(tools[0].name(), "GithubList");
//! ```
//!
//! `cargo run -p haest-issues --example issues` runs a whole agent turn: a
//! scripted model plans work on a board of card files.
//!
//! # Writing your own tools
//!
//! What makes these haest tools, for anyone writing their own (`src/tools.rs`
//! and `src/tools/` are short and meant to be read):
//!
//! - each implements [`haest_core::tool::Tool`]: a name, a description and a
//!   JSON Schema written from what the backend accepts, a canonical `kind`
//!   (`issue.list`, ...), `read_only` for the readers, and `execute`;
//! - a call the model can fix comes back as [`ToolError::Input`] (an unknown
//!   id, a status the tracker does not have), anything else as
//!   [`ToolError::Failed`] (the network, the disk);
//! - a call gives up when the turn is cancelled (`ctx.cancel()`), or after two
//!   minutes;
//! - nothing is read that the host did not hand in: no environment variable,
//!   no path of its own;
//! - the one tool that cannot be undone says so in its `kind`
//!   ([`DELETE_KIND`]), for a host that asks the human first.
//!
//! [`http::HttpClient`] is a plug point in haest's style: the one narrow trait
//! the model calls and the tools send through, a reqwest default, a fake in the tests
//! ([`GithubTracker::with_http`]).
//!
//! [`ToolError::Input`]: haest_core::tool::ToolError::Input
//! [`ToolError::Failed`]: haest_core::tool::ToolError::Failed

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod cards;
mod github;
#[cfg(test)]
mod testkit;
mod tools;
mod tracker;

/// The HTTP seam the GitHub tracker sends through: implement
/// [`http::HttpClient`] to give it a client of your own.
#[doc(inline)]
pub use haest_http as http;

pub use cards::CardTracker;
pub use github::GithubTracker;
pub use tools::{DELETE_KIND, issue_tools, issue_tools_named};
pub use tracker::{
    Issue, IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, Tracker, TrackerError,
    TrackerInfo, one_of,
};
