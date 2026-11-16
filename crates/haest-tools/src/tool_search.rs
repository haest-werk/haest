//! ToolSearch (and, in proxy mode, CallTool): how a model reaches a deferred tool
//! ([`haest_core::deferred_tools`]). ToolSearch's description lists each deferred
//! tool's name and first sentence, so the model knows what exists and what it is
//! for without paying for the schemas.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use haest_core::{
    deferred_tools::{DeferMode, ToolDeferral, search_tools},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, ToolSource, req_str},
};
use serde_json::{Value, json};

/// One loaded schema, as the model reads it: `{"name", "description", "parameters"}`.
fn schema_block(t: &dyn Tool) -> String {
    let s = |v: &Value| serde_json::to_string(v).unwrap_or_default();
    format!(
        "{{\"name\":{},\"description\":{},\"parameters\":{}}}",
        s(&Value::from(t.name())),
        s(&Value::from(t.description())),
        s(&t.parameters())
    )
}

/// What a deferred tool is for, in the line ToolSearch lists it on: its
/// description's first sentence, at most [`HINT_CHARS`]. A bare name list
/// leaves a weaker model guessing which tool to load.
fn hint(description: &str) -> String {
    let line = description.lines().next().unwrap_or("").trim();
    let sentence = line.find(". ").map_or(line, |i| &line[..=i]);
    if sentence.chars().count() <= HINT_CHARS {
        return sentence.to_string();
    }
    let cut: String = sentence.chars().take(HINT_CHARS - 3).collect();
    format!("{}...", cut.trim_end())
}

const HINT_CHARS: usize = 120;

struct ToolSearch {
    deferral: Arc<ToolDeferral>,
    deferred: Vec<Arc<dyn Tool>>,
}

#[async_trait]
impl Tool for ToolSearch {
    fn name(&self) -> &str {
        "ToolSearch"
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        let how = if self.deferral.mode() == DeferMode::Proxy {
            "Then call it through CallTool with {name, input}."
        } else {
            "After loading, call it directly by its name."
        };
        let lines: Vec<String> = self
            .deferred
            .iter()
            .map(|t| match hint(&t.description()) {
                h if h.is_empty() => format!("- {}", t.name()),
                h => format!("- {}: {h}", t.name()),
            })
            .collect();
        format!(
            "Load the full schema of deferred tools before using them. {how} Query \"select:name1,name2\" for exact tools, or keywords to search. Deferred tools:\n{}",
            lines.join("\n")
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"query": {"type": "string", "description": "\"select:a,b\" or keywords"}},
            "required": ["query"]
        })
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let found = search_tools(&self.deferred, req_str(&input, "query")?);
        if found.is_empty() {
            return Err(ToolError::Input(
                "no deferred tool matches; try other keywords or select:<exact name>".into(),
            ));
        }
        self.deferral
            .load(found.iter().map(|t| t.name().to_string()));
        let blocks: Vec<String> = found.iter().map(|t| schema_block(t.as_ref())).collect();
        Ok(ToolOutput::text(format!(
            "Loaded {} tool(s):\n{}",
            found.len(),
            blocks.join("\n")
        )))
    }
}

struct CallTool {
    by_name: HashMap<String, Arc<dyn Tool>>,
}

#[async_trait]
impl Tool for CallTool {
    fn name(&self) -> &str {
        "CallTool"
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn description(&self) -> String {
        "Call a deferred tool by name with its input (load its schema with ToolSearch first)."
            .into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "The deferred tool name"},
                "input": {"type": "object", "description": "Its input, as its schema describes"}
            },
            "required": ["name", "input"]
        })
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = req_str(&input, "name")?;
        let Some(tool) = self.by_name.get(name) else {
            return Err(ToolError::Input(format!(
                "no deferred tool \"{name}\"; ToolSearch lists them"
            )));
        };
        let args = match input.get("input") {
            Some(Value::Object(m)) => m.clone(),
            _ => JsonMap::new(),
        };
        tool.execute(args, ctx).await
    }
}

/// One agent's tool list behind its own deferral: what a host hands the loop
/// (`AgentDeps::tools`) to defer some of its tools.
pub struct DeferredTools {
    tools: Vec<Arc<dyn Tool>>,
    deferral: Arc<ToolDeferral>,
}

impl DeferredTools {
    pub fn new(tools: Vec<Arc<dyn Tool>>, deferral: ToolDeferral) -> Self {
        Self {
            tools,
            deferral: Arc::new(deferral),
        }
    }

    /// Its deferral, e.g. to [`ToolDeferral::load_called`] a resumed history.
    pub fn deferral(&self) -> &Arc<ToolDeferral> {
        &self.deferral
    }
}

impl ToolSource for DeferredTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.deferral
            .advertise(&self.tools, deferral_tools(self.deferral.clone()))
    }
    fn callable(&self) -> Vec<Arc<dyn Tool>> {
        self.deferral
            .callable(&self.tools, deferral_tools(self.deferral.clone()))
    }
}

/// The deferral tools for one agent: ToolSearch, plus CallTool in proxy mode.
/// Pass the result to [`ToolDeferral::advertise`].
pub fn deferral_tools(
    deferral: Arc<ToolDeferral>,
) -> impl Fn(&[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    move |deferred: &[Arc<dyn Tool>]| {
        let search: Arc<dyn Tool> = Arc::new(ToolSearch {
            deferral: deferral.clone(),
            deferred: deferred.to_vec(),
        });
        if deferral.mode() != DeferMode::Proxy {
            return vec![search];
        }
        let by_name = deferred
            .iter()
            .map(|t| (t.name().to_string(), t.clone()))
            .collect();
        vec![search, Arc::new(CallTool { by_name })]
    }
}

#[cfg(test)]
mod tests;
