//! The notebook's side of the tool list, faked: what the node answers.
//!
//! PLUG POINTS: `ReadIntercept` (Read's `attachment:<id>`), `Tool` (the notebook's own
//! tools), `AgentDirectory` (subagents run on the node, not in this process).

use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;
use haest::{
    core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str},
    tools::{
        AgentDirectory, AgentError, AgentRef, ReadIntercept, SpawnInput, SpawnOutcome,
        read_text_output,
    },
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// `attachment:<id>` is no path: the node converts the attachment to Markdown
/// and Read pages it like a file. Every other path falls through to the fs.
pub struct Attachments {
    docs: HashMap<String, String>,
    /// Every path this intercept answered, as the model sent it.
    pub answered: Mutex<Vec<String>>,
}

impl Attachments {
    pub fn new(docs: &[(&str, &str)]) -> Self {
        Self {
            docs: docs
                .iter()
                .map(|(id, md)| (id.to_string(), md.to_string()))
                .collect(),
            answered: Mutex::default(),
        }
    }
}

#[async_trait]
impl ReadIntercept for Attachments {
    async fn intercept(
        &self,
        raw_path: &str,
        input: &JsonMap,
        ctx: &ToolContext,
    ) -> Option<Result<ToolOutput, ToolError>> {
        let id = raw_path.strip_prefix("attachment:")?;
        self.answered.lock().unwrap().push(raw_path.to_string());
        Some(match self.docs.get(id) {
            Some(md) => read_text_output(md, md.len(), input, ctx, ""),
            None => Err(ToolError::Input(format!("no attachment \"{id}\""))),
        })
    }
}

/// notebook_search: the node's ranked full-text search over the notebook.
pub struct NotebookSearch {
    /// The the notebook's index: path, title, text.
    pub notes: Vec<(&'static str, &'static str, &'static str)>,
}

#[async_trait]
impl Tool for NotebookSearch {
    fn name(&self) -> &str {
        "notebook_search"
    }
    fn kind(&self) -> &str {
        "notebook.search"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "Full-text search across the notebook, limited to what the person asking may see.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"query": {"type": "string", "description": "Words to find"}},
            "required": ["query"]
        })
    }
    async fn execute(&self, input: JsonMap, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = req_str(&input, "query")?.to_lowercase();
        let hits: Vec<String> = self
            .notes
            .iter()
            .filter(|(_, _, text)| text.to_lowercase().contains(&query))
            .map(|(path, title, text)| format!("{path} -- {title}\n  {text}"))
            .collect();
        Ok(ToolOutput {
            content: if hits.is_empty() {
                "No results.".into()
            } else {
                hits.join("\n")
            },
            sized: true,
            ..Default::default()
        })
    }
}

/// dream_material: what the dream run reads -- conversations since the last
/// dream and every memory. The node has it; the dream needs no files.
pub struct DreamMaterial;

pub const DREAM_MEMORY: &str = "prefers short weekly summaries";

#[async_trait]
impl Tool for DreamMaterial {
    fn name(&self) -> &str {
        "dream_material"
    }
    fn kind(&self) -> &str {
        "dream.material"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "What the person asked and what you answered since your last dream, and every memory you have of them.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(format!(
            "# Conversations since yesterday\n\nAsked for a Q3 summary.\n\n# Your memories now\n\n{DREAM_MEMORY}"
        )))
    }
}

/// The notebook's agent hub: each subagent runs on its own node run. The fake answers
/// for the node and keeps every spawn, `subagent_type` included (see `agent_options`).
#[derive(Default)]
pub struct NodeAgents {
    pub spawns: Mutex<Vec<(AgentRef, SpawnInput)>>,
}

pub const SUB_ANSWER: &str = "Two related notes: /notebook/notes/plan.md, /notebook/notes/q2.md";
pub const SUB_COST_USD: f64 = 0.002;

#[async_trait]
impl AgentDirectory for NodeAgents {
    async fn spawn(
        &self,
        from: &AgentRef,
        input: SpawnInput,
        _cancel: CancellationToken,
    ) -> Result<SpawnOutcome, AgentError> {
        self.spawns.lock().unwrap().push((from.clone(), input));
        Ok(SpawnOutcome {
            text: SUB_ANSWER.into(),
            cost_usd: SUB_COST_USD,
        })
    }
    fn deliver(&self, _from: &str, to: &str, _text: &str) -> Result<String, AgentError> {
        Err(AgentError::Refused(format!("\"{to}\" is not running")))
    }
    fn stop(&self, _name: &str) -> bool {
        false
    }
}
