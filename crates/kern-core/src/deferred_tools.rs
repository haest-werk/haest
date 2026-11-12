//! DEFERRED TOOLS: MCP tools stay out of the request until the model asks for
//! them. A conversation with a large MCP server connected carries dozens of tool
//! schemas on every call, used a few at a time; the model is told the NAMES
//! (ToolSearch's description) and loads a schema when it needs one.
//!
//! Two ways to make a loaded tool callable, an experiment:
//!   proxy  one fixed CallTool(name, input) runs any deferred tool. The tool list
//!          never changes, so the prompt cache prefix never breaks.
//!   grow   a loaded tool joins the real tool list. Native calls, but every load
//!          changes the list -- and tools sit at the FRONT of the cached prefix.
//!   off    every tool, every call (the old behaviour).
//!
//! What defers: MCP tools by default (built-ins are few, small and used
//! constantly). A host with many tools of its own defers every tool but a list
//! it always needs ([`ToolDeferral::deferring_all_except`]).
//!
//! A deferred tool the model calls by name, without ToolSearch, still runs
//! ([`ToolDeferral::callable`]) and counts as loaded: a prompt that names a tool,
//! or a resumed conversation that called one ([`ToolDeferral::load_called`]),
//! never meets "Unknown tool".
//!
//! The ToolSearch / CallTool tools themselves live in `kern-tools` (`tool_search`).

use std::{collections::HashSet, sync::Arc, sync::Mutex};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    messages::Message,
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
};

crate::word_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DeferMode ("deferred-tools mode") {
        Off = "off",
        Proxy = "proxy",
        Grow = "grow",
    }
}

const MAX_MATCHES: usize = 5;

type Tools = Vec<Arc<dyn Tool>>;

/// MCP tools a conversation host needs on every turn, by name suffix: a reply to
/// another conversation must not wait on a ToolSearch.
pub const DEFAULT_ALWAYS_LOADED: [&str; 2] = ["__send_message", "__list_conversations"];

/// Exact `select:a,b`, else keyword match over name + description, best first (at most 5).
pub fn search_tools(pool: &[Arc<dyn Tool>], query: &str) -> Vec<Arc<dyn Tool>> {
    let q = query.trim();
    if let Some(list) = q.strip_prefix("select:") {
        let wanted: HashSet<&str> = list.split(',').map(str::trim).collect();
        return pool
            .iter()
            .filter(|t| wanted.contains(t.name()))
            .cloned()
            .collect();
    }
    let q = q.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(usize, &Arc<dyn Tool>)> = pool
        .iter()
        .map(|t| {
            let name = t.name().to_lowercase();
            let text = format!("{name} {}", t.description().to_lowercase());
            let score = words
                .iter()
                .map(|w| usize::from(name.contains(w)) * 3 + usize::from(text.contains(w)))
                .sum();
            (score, t)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    // Stable: equal scores keep pool order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored
        .into_iter()
        .take(MAX_MATCHES)
        .map(|(_, t)| t.clone())
        .collect()
}

/// One agent's deferral state: which deferred tools it has loaded. Shared (`Arc`)
/// between the host's tool source and the ToolSearch tool that loads them.
pub struct ToolDeferral {
    mode: DeferMode,
    loaded: Mutex<HashSet<String>>,
    always_loaded: Vec<String>,
    /// `Some`: every tool defers but these names. `None`: MCP tools only.
    all_but: Option<HashSet<String>>,
}

impl ToolDeferral {
    pub fn new(mode: DeferMode) -> Self {
        Self {
            mode,
            loaded: Mutex::default(),
            always_loaded: DEFAULT_ALWAYS_LOADED.map(String::from).to_vec(),
            all_but: None,
        }
    }

    /// Name suffixes of MCP tools that are never deferred (replaces the default).
    pub fn with_always_loaded(mut self, suffixes: impl IntoIterator<Item = String>) -> Self {
        self.always_loaded = suffixes.into_iter().collect();
        self
    }

    /// Defer EVERY tool, the host's own too, except these names (the ones
    /// nearly every turn uses). MCP tools matching [`Self::with_always_loaded`] stay too.
    pub fn deferring_all_except(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.all_but = Some(names.into_iter().collect());
        self
    }

    pub fn mode(&self) -> DeferMode {
        self.mode
    }

    /// Mark tools loaded (ToolSearch did). Only `grow` changes the advertised list because of it.
    pub fn load<S: Into<String>>(&self, names: impl IntoIterator<Item = S>) {
        let mut loaded = self.loaded.lock().unwrap();
        loaded.extend(names.into_iter().map(Into::into));
    }

    /// Every tool `history` called counts as loaded: a resumed conversation
    /// keeps the tools it was using.
    pub fn load_called(&self, history: &[Message]) {
        let called = history.iter().flat_map(|m| match m {
            Message::Assistant(a) => a.tool_calls().map(|c| c.name.clone()).collect(),
            _ => Vec::new(),
        });
        self.load(called);
    }

    fn is_deferrable(&self, t: &dyn Tool) -> bool {
        let name = t.name();
        let mcp = name.starts_with("mcp__");
        if mcp
            && self
                .always_loaded
                .iter()
                .any(|s| name.ends_with(s.as_str()))
        {
            return false;
        }
        match &self.all_but {
            Some(keep) => !keep.contains(name),
            None => mcp,
        }
    }

    /// `all` split into (deferred, eager); `None` when nothing defers.
    fn split(&self, all: &[Arc<dyn Tool>]) -> Option<(Tools, Tools)> {
        if self.mode == DeferMode::Off {
            return None;
        }
        let (deferred, eager): (Vec<_>, Vec<_>) = all
            .iter()
            .cloned()
            .partition(|t| self.is_deferrable(t.as_ref()));
        (!deferred.is_empty()).then_some((deferred, eager))
    }

    /// Every tool a call may run: the advertised ones and every deferred one,
    /// which loads itself when the model calls it by name.
    pub fn callable(
        self: &Arc<Self>,
        all: &[Arc<dyn Tool>],
        deferral_tools: impl FnOnce(&[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>>,
    ) -> Vec<Arc<dyn Tool>> {
        let Some((deferred, mut out)) = self.split(all) else {
            return all.to_vec();
        };
        out.extend(deferral_tools(&deferred));
        out.extend(deferred.into_iter().map(|tool| {
            Arc::new(Loading {
                tool,
                deferral: self.clone(),
            }) as Arc<dyn Tool>
        }));
        out
    }

    /// The tools this call advertises, given every tool the agent has and the deferral tools to add.
    pub fn advertise(
        &self,
        all: &[Arc<dyn Tool>],
        deferral_tools: impl FnOnce(&[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>>,
    ) -> Vec<Arc<dyn Tool>> {
        let Some((deferred, eager)) = self.split(all) else {
            return all.to_vec();
        };
        let grown: Vec<Arc<dyn Tool>> = if self.mode == DeferMode::Grow {
            let loaded = self.loaded.lock().unwrap();
            deferred
                .iter()
                .filter(|t| loaded.contains(t.name()))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let mut out = eager;
        out.extend(deferral_tools(&deferred));
        out.extend(grown);
        out
    }
}

/// A deferred tool as a call runs it: called by name, it counts as loaded.
struct Loading {
    tool: Arc<dyn Tool>,
    deferral: Arc<ToolDeferral>,
}

#[async_trait]
impl Tool for Loading {
    fn name(&self) -> &str {
        self.tool.name()
    }
    fn description(&self) -> String {
        self.tool.description()
    }
    fn parameters(&self) -> Value {
        self.tool.parameters()
    }
    fn kind(&self) -> &str {
        self.tool.kind()
    }
    fn read_only(&self) -> bool {
        self.tool.read_only()
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        self.tool.canonical_input(input)
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.deferral.load([self.tool.name()]);
        self.tool.execute(input, ctx).await
    }
}

#[cfg(test)]
mod tests;
