//! The four tools that change the tracker: IssueCreate, IssueUpdate,
//! IssueDelete (the one that cannot be undone) and IssueLink.

use async_trait::async_trait;
use haest_core::tool::{
    JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_bool, opt_str, req_str,
};
use serde_json::{Value, json};

use super::{DELETE_KIND, Shared, id_in, line, run, words_in};
use crate::tracker::{IssuePatch, Link, LinkChange, NewIssue, Relation};

issue_tool!(IssueCreateTool);
issue_tool!(IssueUpdateTool);
issue_tool!(IssueDeleteTool);
issue_tool!(IssueLinkTool);

fn opt_string(input: &JsonMap, key: &str) -> Result<Option<String>, ToolError> {
    Ok(opt_str(input, key)?.map(str::to_string))
}

#[async_trait]
impl Tool for IssueCreateTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        "issue.create"
    }
    fn description(&self) -> String {
        format!(
            "Create an issue in {}. A new one starts as \"{}\" unless you give a status. {}",
            self.s.info.name,
            self.s.info.statuses.first().map_or("", String::as_str),
            self.s.accepts()
        )
    }
    fn parameters(&self) -> Value {
        let mut props = JsonMap::new();
        props.insert(
            "title".into(),
            json!({"type": "string", "description": "A short title"}),
        );
        props.insert(
            "body".into(),
            json!({"type": "string", "description": "The description, markdown"}),
        );
        props.insert("status".into(), self.s.status_param("Its status", false));
        self.s.priority_param(&mut props);
        props.insert("labels".into(), Shared::labels_param("Its labels"));
        json!({"type": "object", "properties": props, "required": ["title"]})
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let new = NewIssue {
            title: req_str(&input, "title")?.to_string(),
            body: opt_string(&input, "body")?.unwrap_or_default(),
            status: opt_string(&input, "status")?,
            priority: opt_string(&input, "priority")?,
            labels: words_in(&input, "labels")?,
        };
        let made = run(ctx, self.s.tracker.create(new)).await?;
        let at = made
            .url
            .as_deref()
            .map_or(String::new(), |u| format!("\n{u}"));
        Ok(ToolOutput::text(format!("Created {}{at}", line(&made))))
    }
}

#[async_trait]
impl Tool for IssueUpdateTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        "issue.update"
    }
    fn description(&self) -> String {
        format!(
            "Change an issue of {}: its title, body (the whole new text), status, priority or labels. `labels` replaces the whole set; `add_labels` and `remove_labels` change only those. Leave out what stays as it is. {}",
            self.s.info.name,
            self.s.accepts()
        )
    }
    fn parameters(&self) -> Value {
        let mut props = JsonMap::new();
        props.insert("id".into(), self.s.id_param());
        props.insert("title".into(), json!({"type": "string"}));
        props.insert(
            "body".into(),
            json!({"type": "string", "description": "The whole new description"}),
        );
        props.insert(
            "status".into(),
            self.s.status_param("The new status", false),
        );
        self.s.priority_param(&mut props);
        props.insert(
            "labels".into(),
            Shared::labels_param("The whole new set of labels"),
        );
        props.insert("add_labels".into(), Shared::labels_param("Labels to add"));
        props.insert(
            "remove_labels".into(),
            Shared::labels_param("Labels to take off"),
        );
        json!({"type": "object", "properties": props, "required": ["id"]})
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = id_in(&input, "id")?;
        let replace = match input.get("labels") {
            None | Some(Value::Null) => None,
            Some(_) => Some(words_in(&input, "labels")?),
        };
        let patch = IssuePatch {
            title: opt_string(&input, "title")?,
            body: opt_string(&input, "body")?,
            status: opt_string(&input, "status")?,
            priority: opt_string(&input, "priority")?,
            labels: replace,
            add_labels: words_in(&input, "add_labels")?,
            remove_labels: words_in(&input, "remove_labels")?,
        };
        let changed = run(ctx, self.s.tracker.update(&id, patch)).await?;
        Ok(ToolOutput::text(format!("Updated {}", line(&changed))))
    }
}

#[async_trait]
impl Tool for IssueDeleteTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        DELETE_KIND
    }
    fn description(&self) -> String {
        format!(
            "Delete an issue of {} for good. This cannot be undone: there is no trash and no restore. Only when the human asked for it; to finish or drop work, change its status instead.",
            self.s.info.name
        )
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"id": self.s.id_param()}, "required": ["id"]})
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = id_in(&input, "id")?;
        run(ctx, self.s.tracker.delete(&id)).await?;
        Ok(ToolOutput::text(format!("Deleted {id} for good.")))
    }
}

/// What each relation means, for the description.
fn meaning(r: Relation) -> &'static str {
    match r {
        Relation::Parent => {
            "parent (the target is this one's parent; an issue has one parent, a new one replaces it)"
        }
        Relation::Child => "child (the target becomes a child of this one)",
        Relation::DependsOn => "depends_on (the target must be done first)",
        Relation::Blocks => "blocks (the target waits on this one)",
        Relation::RelatesTo => "relates_to (worth reading together; no order)",
    }
}

#[async_trait]
impl Tool for IssueLinkTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        "issue.link"
    }
    fn description(&self) -> String {
        let relations: Vec<&str> = self.s.info.relations.iter().map(|r| meaning(*r)).collect();
        format!(
            "Link one issue of {} to another, or take a link off (remove: true). Relations, read as \"<id> <relation> <target>\": {}.",
            self.s.info.name,
            relations.join("; ")
        )
    }
    fn parameters(&self) -> Value {
        let words: Vec<&str> = self.s.info.relations.iter().map(|r| r.as_str()).collect();
        json!({
            "type": "object",
            "properties": {
                "id": self.s.id_param(),
                "relation": {"type": "string", "enum": words},
                "target": {"type": "string", "description": "The other issue's id"},
                "remove": {"type": "boolean", "description": "Take the link off instead of adding it"}
            },
            "required": ["id", "relation", "target"]
        })
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = id_in(&input, "id")?;
        let target = id_in(&input, "target")?;
        let word = req_str(&input, "relation")?;
        let relation = (word.parse::<Relation>().ok())
            .filter(|r| self.s.info.relations.contains(r))
            .ok_or_else(|| {
                let known: Vec<&str> = self.s.info.relations.iter().map(|r| r.as_str()).collect();
                ToolError::Input(format!(
                    "\"{word}\" is not a relation here; use one of: {}",
                    known.join(", ")
                ))
            })?;
        let (change, done) = if opt_bool(&input, "remove")? {
            (LinkChange::Remove, "Unlinked")
        } else {
            (LinkChange::Add, "Linked")
        };
        let link = Link::new(relation, target.clone());
        run(ctx, self.s.tracker.link(&id, &link, change)).await?;
        Ok(ToolOutput::text(format!(
            "{done}: {id} {} {target}",
            relation.as_str()
        )))
    }
}
