//! ToolSearch (and, in proxy mode, CallTool): how a model reaches a deferred tool
//! ([`kern_core::deferred_tools`]). ToolSearch's description carries the deferred
//! tool NAMES, so the model knows what exists without paying for the schemas.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use kern_core::{
    deferred_tools::{DeferMode, ToolDeferral, search_tools},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str},
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
        let names: Vec<&str> = self.deferred.iter().map(|t| t.name()).collect();
        format!(
            "Load the full schema of deferred tools before using them. {how} Query \"select:name1,name2\" for exact tools, or keywords to search. Deferred tools: {}",
            names.join(", ")
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
