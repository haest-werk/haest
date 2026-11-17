//! ToolSearch (and, in proxy mode, CallTool): how a model reaches a deferred tool
//! ([`haest_core::deferred_tools`]). ToolSearch's description lists each deferred
//! tool's name and first sentence, so the model knows what exists and what it is
//! for without paying for the schemas.
//!
//! Start from [`DeferredTools`]: a [`ToolSource`] behind a [`ToolDeferral`],
//! handed to the loop as its own source. Which tools are deferred, and when one
//! counts as loaded, is the deferral's (haest-core).

use std::collections::HashMap;

use async_trait::async_trait;
use haest_core::{
    __private::{advertised_tools, callable_tools},
    ToolRef, ToolSourceRef, Tools,
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

#[derive(Debug)]
struct ToolSearchTool {
    deferral: ToolDeferral,
    deferred: Tools,
}

#[async_trait]
impl Tool for ToolSearchTool {
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

#[derive(Debug)]
struct CallToolTool {
    by_name: HashMap<String, ToolRef>,
}

#[async_trait]
impl Tool for CallToolTool {
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

/// One agent's tools behind its own deferral: what a host hands the loop
/// ([`AgentDeps::tools`](haest_core::AgentDeps::tools)) to defer some of its
/// tools.
///
/// Before every model call it asks the inner [`ToolSource`] for its tools and
/// advertises the ones the deferral does not hold back, plus ToolSearch (and
/// CallTool in proxy mode) over the ones it does. A deferred tool the model
/// calls by name still runs ([`ToolSource::callable`]). The inner source may
/// be a fixed `Vec` or one whose tools change between calls.
///
/// # Examples
///
/// ```
/// use haest_core::{
///     deferred_tools::{DeferMode, ToolDeferral},
///     tool::ToolSource,
/// };
/// use haest_tools::{BuiltinOptions, DeferredTools, builtin_tools};
///
/// let tools = builtin_tools(BuiltinOptions::new().without_web_fetch());
/// // Proxy mode, every tool deferred but Read.
/// let deferral = ToolDeferral::new(DeferMode::Proxy).deferring_all_except(["Read"]);
/// let source = DeferredTools::new(tools, deferral);
/// let advertised: Vec<String> = source.tools().iter().map(|t| t.name().to_string()).collect();
/// assert_eq!(advertised, ["Read", "ToolSearch", "CallTool"]);
/// // Called by name anyway, a deferred tool still runs.
/// assert!(source.callable().iter().any(|t| t.name() == "Bash"));
/// ```
///
/// Clones share the source and the deferral: a schema one loads, every
/// clone advertises.
#[derive(Debug, Clone)]
pub struct DeferredTools {
    source: ToolSourceRef,
    deferral: ToolDeferral,
}

impl DeferredTools {
    /// The tools `source` has, deferred as `deferral` says.
    pub fn new(source: impl Into<ToolSourceRef>, deferral: ToolDeferral) -> Self {
        Self {
            source: source.into(),
            deferral,
        }
    }

    /// Its deferral, e.g. to [`ToolDeferral::load_called`] a resumed history.
    /// A clone is the same deferral.
    pub fn deferral(&self) -> &ToolDeferral {
        &self.deferral
    }
}

impl ToolSource for DeferredTools {
    fn tools(&self) -> Tools {
        advertised_tools(
            &self.deferral,
            &self.source.tools(),
            deferral_tools(self.deferral.clone()),
        )
    }
    fn callable(&self) -> Tools {
        callable_tools(
            &self.deferral,
            &self.source.callable(),
            deferral_tools(self.deferral.clone()),
        )
    }
}

/// The deferral tools for one agent: ToolSearch, plus CallTool in proxy mode.
///
/// The returned function takes the deferred tools and makes the deferral
/// tools over them: ToolSearch (read-only, kind `agent.unknown`) loads the
/// schemas a `query` names (`select:a,b`, or keywords) into the deferral;
/// CallTool runs a deferred tool by `name` with its `input`.
fn deferral_tools(deferral: ToolDeferral) -> impl Fn(&[ToolRef]) -> Tools {
    move |deferred: &[ToolRef]| {
        let search = ToolRef::new(ToolSearchTool {
            deferral: deferral.clone(),
            deferred: deferred.iter().cloned().collect(),
        });
        if deferral.mode() != DeferMode::Proxy {
            return Tools::new().with(search);
        }
        let by_name = deferred
            .iter()
            .map(|t| (t.name().to_string(), t.clone()))
            .collect();
        Tools::new().with(search).with(CallToolTool { by_name })
    }
}

#[cfg(test)]
mod tests;
