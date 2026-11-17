//! The issue tools: seven haest [`Tool`]s over one [`Tracker`].
//!
//! | Tool | Kind | Read only | Does |
//! |---|---|---|---|
//! | IssueList | `issue.list` | yes | issues by status, labels, priority |
//! | IssueSearch | `issue.search` | yes | issues by words |
//! | IssueGet | `issue.get` | yes | one issue in full, links included |
//! | IssueCreate | `issue.create` | no | a new issue |
//! | IssueUpdate | `issue.update` | no | title, body, status, priority, labels |
//! | IssueDelete | `issue.delete` | no | deletes for good: THE DESTRUCTIVE ONE |
//! | IssueLink | `issue.link` | no | adds or removes a relation |
//!
//! haest's tool contract has no "destructive" flag: a tool says `read_only`
//! (false here for every writer) and its canonical `kind`. IssueDelete is the
//! one whose effect cannot be undone; its kind is [`DELETE_KIND`], and a host
//! that asks the human before irreversible calls gates on that kind.
//!
//! Each tool's description and schema are written from the tracker's
//! [`TrackerInfo`]: the statuses, priorities and relations it accepts.
//!
//! Every call gives up when the turn is cancelled (an input error the model
//! reads as "interrupted") or after two minutes (a failure). A tracker's
//! `NotFound` and `Invalid` reach the model as input errors it can fix,
//! `Failed` as a failure. Ids may come as strings or numbers (`42`), and a
//! list of labels as an array or one comma-separated string.

use std::{future::Future, sync::Arc, time::Duration};

use haest_core::tool::{JsonMap, ToolContext, ToolError, opt_num, spelled};
use serde_json::{Value, json};

use crate::tracker::{Issue, IssueFilter, Tracker, TrackerError, TrackerInfo};
use haest_core::{ToolRef, Tools};

/// A tool's struct: its name and the tracker it shares with the others.
macro_rules! issue_tool {
    ($tool:ident) => {
        pub(super) struct $tool {
            name: String,
            s: std::sync::Arc<super::Shared>,
        }

        impl $tool {
            pub(super) fn new(name: String, s: std::sync::Arc<super::Shared>) -> Self {
                Self { name, s }
            }
        }
    };
}

mod read;
mod write;

/// The kind of IssueDelete, the one tool whose effect cannot be undone: a
/// host that asks the human before irreversible calls gates on it. (haest's
/// built-in approval mod judges Bash only.)
pub const DELETE_KIND: &str = "issue.delete";

/// How long one tool call may take, every request of it together.
const TIMEOUT: Duration = Duration::from_secs(120);
const MAX_LIMIT: usize = 100;

/// What every issue tool shares: the tracker and what it accepts.
pub(crate) struct Shared {
    tracker: Arc<dyn Tracker>,
    info: TrackerInfo,
}

/// The seven issue tools over `tracker`, named `Issue*`: IssueList,
/// IssueSearch, IssueGet, IssueCreate, IssueUpdate, IssueDelete, IssueLink,
/// in that order.
///
/// Put them in the agent's tool list beside its others. The [crate
/// docs](crate#examples) call them on a board of cards.
pub fn issue_tools(tracker: Arc<dyn Tracker>) -> Tools {
    issue_tools_named(tracker, "Issue")
}

/// The seven tools named `<noun>List`, `<noun>Get`, ...: two trackers on one
/// agent need two nouns (`Issue` for GitHub, `Card` for the board).
///
/// [`TrackerInfo`] is read once, here: the tools' descriptions and schemas
/// do not follow a tracker that changes what it accepts later.
pub fn issue_tools_named(tracker: Arc<dyn Tracker>, noun: &str) -> Tools {
    let shared = Arc::new(Shared {
        info: tracker.info(),
        tracker,
    });
    let named = |verb: &str| format!("{noun}{verb}");
    Tools::from_iter([
        ToolRef::new(read::IssueListTool::new(named("List"), shared.clone())),
        ToolRef::new(read::IssueSearchTool::new(named("Search"), shared.clone())),
        ToolRef::new(read::IssueGetTool::new(named("Get"), shared.clone())),
        ToolRef::new(write::IssueCreateTool::new(named("Create"), shared.clone())),
        ToolRef::new(write::IssueUpdateTool::new(named("Update"), shared.clone())),
        ToolRef::new(write::IssueDeleteTool::new(named("Delete"), shared.clone())),
        ToolRef::new(write::IssueLinkTool::new(named("Link"), shared)),
    ])
}

/// What the model can fix goes back as an input error; the rest as a failure.
fn tool_error(e: TrackerError) -> ToolError {
    match e {
        TrackerError::NotFound(m) | TrackerError::Invalid(m) => ToolError::Input(m),
        TrackerError::Failed(m) => ToolError::Failed(m),
    }
}

/// Run one tracker call, giving up when the turn is cancelled or time runs out.
async fn run<T>(
    ctx: &ToolContext,
    call: impl Future<Output = Result<T, TrackerError>>,
) -> Result<T, ToolError> {
    tokio::select! {
        _ = ctx.cancel().cancelled() => Err(ToolError::Input("the call was interrupted".into())),
        done = tokio::time::timeout(TIMEOUT, call) => match done {
            Ok(result) => result.map_err(tool_error),
            Err(_) => Err(ToolError::Failed(format!(
                "the tracker did not answer within {}s",
                TIMEOUT.as_secs()
            ))),
        },
    }
}

/// An id, as a string or a number (models send `42` for an issue number).
fn id_in(input: &JsonMap, key: &str) -> Result<String, ToolError> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        _ => Err(ToolError::Input(format!("\"{key}\" is required"))),
    }
}

/// A list of words: an array, or one string of comma-separated words.
fn words_in(input: &JsonMap, key: &str) -> Result<Vec<String>, ToolError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items
            .iter()
            .map(|v| spelled(v).trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()),
        Some(Value::String(s)) => Ok(s
            .split(',')
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect()),
        Some(_) => Err(ToolError::Input(format!(
            "\"{key}\" must be a list of strings"
        ))),
    }
}

/// `limit`, 1 to 100; 30 when not given.
fn limit_in(input: &JsonMap) -> Result<usize, ToolError> {
    let n = opt_num(input, "limit")?.map_or(IssueFilter::DEFAULT_LIMIT, |n| n.max(1.0) as usize);
    Ok(n.min(MAX_LIMIT))
}

impl Shared {
    fn id_param(&self) -> Value {
        json!({"type": "string", "description": self.info.id_hint})
    }

    fn labels_param(description: &str) -> Value {
        json!({"type": "array", "items": {"type": "string"}, "description": description})
    }

    /// The `status` property; `all` adds the word that lists every status.
    fn status_param(&self, description: &str, all: bool) -> Value {
        let mut words = self.info.statuses.clone();
        if all {
            words.push("all".into());
        }
        json!({"type": "string", "enum": words, "description": description})
    }

    /// Add `priority` to `props` when this tracker has priorities.
    fn priority_param(&self, props: &mut JsonMap) {
        if !self.info.priorities.is_empty() {
            props.insert(
                "priority".into(),
                json!({"type": "string", "enum": self.info.priorities}),
            );
        }
    }

    /// "statuses a, b; priorities c, d" for a description.
    fn accepts(&self) -> String {
        let mut s = format!("Statuses: {}.", self.info.statuses.join(", "));
        if !self.info.priorities.is_empty() {
            s.push_str(&format!(
                " Priorities: {}.",
                self.info.priorities.join(", ")
            ));
        }
        s
    }
}

/// One issue on one line: `id [status] title, priority p, labels: a, b`.
fn line(i: &Issue) -> String {
    let mut s = format!("{} [{}] {}", i.id, i.status, i.title);
    if let Some(p) = &i.priority {
        s.push_str(&format!(", priority {p}"));
    }
    if !i.labels.is_empty() {
        s.push_str(&format!(", labels: {}", i.labels.join(", ")));
    }
    s
}

/// Issues one per line, saying when the list may go on.
fn lines(issues: &[Issue], limit: usize) -> String {
    if issues.is_empty() {
        return "No issues.".into();
    }
    let mut out: Vec<String> = issues.iter().map(line).collect();
    if issues.len() >= limit {
        out.push(format!(
            "(the first {limit}; there may be more: raise limit or narrow the filter)"
        ));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests;
