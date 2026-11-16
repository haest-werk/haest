//! The three tools that only read: IssueList, IssueSearch, IssueGet.

use async_trait::async_trait;
use haest_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str};
use serde_json::{Value, json};

use super::{Shared, id_in, limit_in, line, lines, run, words_in};
use crate::tracker::{Issue, IssueFilter};

issue_tool!(IssueListTool);

#[async_trait]
impl Tool for IssueListTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        "issue.list"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        format!(
            "List {}, newest first: each one's id, status, title and labels. With no status it lists the open ones ({}); \"all\" lists every status. {}",
            self.s.info.name,
            self.s.info.open_statuses.join(", "),
            self.s.accepts()
        )
    }
    fn parameters(&self) -> Value {
        let mut props = JsonMap::new();
        props.insert(
            "status".into(),
            self.s
                .status_param("Only this status; \"all\" for every one", true),
        );
        props.insert(
            "labels".into(),
            Shared::labels_param("Only issues with every one of these labels"),
        );
        self.s.priority_param(&mut props);
        props.insert(
            "limit".into(),
            json!({"type": "number", "description": "At most this many (1-100, default 30)"}),
        );
        json!({"type": "object", "properties": props})
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let statuses = match opt_str(&input, "status")? {
            None => None,
            Some("all") => Some(self.s.info.statuses.clone()),
            Some(one) => Some(vec![one.to_string()]),
        };
        let filter = IssueFilter {
            statuses,
            labels: words_in(&input, "labels")?,
            priority: opt_str(&input, "priority")?.map(str::to_string),
            limit: limit_in(&input)?,
        };
        let found = run(ctx, self.s.tracker.list(&filter)).await?;
        Ok(ToolOutput::text(lines(&found, filter.limit)))
    }
}

issue_tool!(IssueSearchTool);

#[async_trait]
impl Tool for IssueSearchTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        "issue.search"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        format!(
            "Search {} for words in the title, body and labels, in every status. Returns one line per issue; get one to read it in full.",
            self.s.info.name
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "The words to find"},
                "limit": {"type": "number", "description": "At most this many (1-100, default 30)"}
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = req_str(&input, "query")?;
        let limit = limit_in(&input)?;
        let found = run(ctx, self.s.tracker.search(query, limit)).await?;
        Ok(ToolOutput::text(lines(&found, limit)))
    }
}

issue_tool!(IssueGetTool);

/// An issue in full: its line, its fields, its links, its body.
pub(super) fn full(i: &Issue) -> String {
    let mut out = vec![line(i)];
    let mut field = |k: &str, v: &Option<String>| {
        if let Some(v) = v {
            out.push(format!("{k}: {v}"));
        }
    };
    field("url", &i.url);
    field("created", &i.created);
    field("updated", &i.updated);
    for (relation, targets) in grouped_links(i) {
        out.push(format!("{relation}: {}", targets.join(", ")));
    }
    for (k, v) in &i.extra {
        out.push(format!("{k}: {v}"));
    }
    if !i.body.is_empty() {
        out.push(String::new());
        out.push(i.body.clone());
    }
    out.join("\n")
}

/// The links by relation, in the order relations are listed.
fn grouped_links(i: &Issue) -> Vec<(&'static str, Vec<&str>)> {
    crate::tracker::Relation::ALL
        .iter()
        .filter_map(|&r| {
            let targets: Vec<&str> = (i.links.iter())
                .filter(|l| l.relation == r)
                .map(|l| l.target.as_str())
                .collect();
            (!targets.is_empty()).then_some((r.as_str(), targets))
        })
        .collect()
}

#[async_trait]
impl Tool for IssueGetTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        "issue.get"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        format!(
            "One issue of {} in full: its description, labels, links (parent, child, depends_on, blocks, relates_to) and other fields.",
            self.s.info.name
        )
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"id": self.s.id_param()}, "required": ["id"]})
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = id_in(&input, "id")?;
        let issue = run(ctx, self.s.tracker.get(&id)).await?;
        Ok(ToolOutput::text(full(&issue)))
    }
}
