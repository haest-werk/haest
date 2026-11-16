//! DEFERRED TOOLS: MCP tools stay out of the request until the model asks for
//! them. A conversation with a large MCP server connected carries dozens of tool
//! schemas on every call, used a few at a time; the model is told the NAMES
//! (ToolSearch's description) and loads a schema when it needs one.
//!
//! Two ways to make a loaded tool callable, an experiment ([`DeferMode`]):
//!
//! - `proxy`: one fixed CallTool(name, input) runs any deferred tool. The tool list
//!   never changes, so the prompt cache prefix never breaks.
//! - `grow`: a loaded tool joins the real tool list. Native calls, but every load
//!   changes the list -- and tools sit at the FRONT of the cached prefix.
//! - `off`: every tool, every call (the old behaviour).
//!
//! [`ToolDeferral`] holds one agent's state. A host's
//! [`ToolSource`](crate::ToolSource) asks it
//! which tools to advertise ([`ToolDeferral::advertise`]) and which a call may
//! run ([`ToolDeferral::callable`]); the ToolSearch tool calls
//! [`search_tools`] and [`ToolDeferral::load`].
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
//! The ToolSearch / CallTool tools themselves live in `haest-tools` (`tool_search`).

use std::{collections::HashSet, sync::Arc, sync::Mutex};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    messages::Message,
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
};

crate::word_enum! {
    /// How deferred tools reach the model; see the [module docs](self).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DeferMode ("deferred-tools mode") {
        /// Nothing defers: every tool is advertised on every call.
        Off = "off",
        /// Deferred tools run through one fixed CallTool; the tool list never changes.
        Proxy = "proxy",
        /// A loaded tool joins the advertised list.
        Grow = "grow",
    }
}

const MAX_MATCHES: usize = 5;

type Tools = Vec<Arc<dyn Tool>>;

/// Exact `select:a,b`, else keyword match over name + description, best first (at most 5).
///
/// `select:` returns the named tools in `pool` order, unknown names skipped.
/// A keyword query scores each tool per word: 3 for a hit in the name, 1 for
/// a hit anywhere in name or description, case-insensitive; ties keep `pool`
/// order and tools that score nothing are left out.
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
///
/// By default MCP tools (names starting `mcp__`) defer, except those a host
/// keeps loaded ([`with_always_loaded`](Self::with_always_loaded)).
///
/// # Examples
///
/// A host's tool source that defers through it:
///
/// ```
/// # use std::sync::Arc;
/// # use async_trait::async_trait;
/// # use haest_core::{ToolContext, ToolError, ToolOutput, tool::JsonMap};
/// # struct Named(&'static str);
/// # #[async_trait]
/// # impl Tool for Named {
/// #     fn name(&self) -> &str { self.0 }
/// #     fn description(&self) -> String { String::new() }
/// #     fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "example" }
/// #     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
/// #         Ok(ToolOutput::text("ok"))
/// #     }
/// # }
/// use haest_core::{
///     Tool, ToolSource,
///     deferred_tools::{DeferMode, ToolDeferral},
/// };
///
/// struct Tools {
///     all: Vec<Arc<dyn Tool>>,
///     deferral: Arc<ToolDeferral>,
/// }
///
/// impl ToolSource for Tools {
///     // A real host adds its ToolSearch tool in the closures (haest-tools has one).
///     fn tools(&self) -> Vec<Arc<dyn Tool>> {
///         self.deferral.advertise(&self.all, |_deferred| Vec::new())
///     }
///     fn callable(&self) -> Vec<Arc<dyn Tool>> {
///         self.deferral.callable(&self.all, |_deferred| Vec::new())
///     }
/// }
///
/// let all: Vec<Arc<dyn Tool>> = vec![Arc::new(Named("Read")), Arc::new(Named("mcp__docs__search"))];
/// let source = Tools { all, deferral: Arc::new(ToolDeferral::new(DeferMode::Grow)) };
/// let names = |tools: Vec<Arc<dyn Tool>>| -> Vec<String> {
///     tools.iter().map(|t| t.name().to_string()).collect()
/// };
///
/// // The MCP tool is not advertised, but a call to it by name still runs.
/// assert_eq!(names(source.tools()), ["Read"]);
/// assert_eq!(names(source.callable()), ["Read", "mcp__docs__search"]);
///
/// // Once loaded, `grow` advertises it.
/// source.deferral.load(["mcp__docs__search"]);
/// assert_eq!(names(source.tools()), ["Read", "mcp__docs__search"]);
/// ```
pub struct ToolDeferral {
    mode: DeferMode,
    loaded: Mutex<HashSet<String>>,
    always_loaded: Vec<String>,
    /// `Some`: every tool defers but these names. `None`: MCP tools only.
    all_but: Option<HashSet<String>>,
}

impl ToolDeferral {
    /// Deferral in `mode`, nothing loaded yet, MCP tools deferring.
    pub fn new(mode: DeferMode) -> Self {
        Self {
            mode,
            loaded: Mutex::default(),
            always_loaded: Vec::new(),
            all_but: None,
        }
    }

    /// Name suffixes of MCP tools that are never deferred: the ones a turn
    /// must not wait on a ToolSearch for (a host's `__send_message`, so a
    /// reply to another conversation goes out at once). None by default.
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

    /// The mode it was made with.
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
        let answers = history.iter().filter_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        });
        self.load(answers.flat_map(|a| a.tool_calls()).map(|c| c.name.clone()));
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
    ///
    /// `deferral_tools` builds the tools that reach deferred ones (ToolSearch,
    /// and CallTool for `proxy`) from the deferred list; it is called only when
    /// something defers. With nothing deferring this is `all`.
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
    ///
    /// The tools that do not defer, in `all` order, then what
    /// `deferral_tools` returns, then (in `grow`) the loaded deferred ones.
    /// With nothing deferring this is `all`, and `deferral_tools` is not called.
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
