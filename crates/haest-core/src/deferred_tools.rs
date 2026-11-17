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
//! [`ToolDeferral`] holds one agent's state. haest-tools' `DeferredTools`
//! asks it which tools to advertise and which a call may run, over any
//! [`ToolSource`](crate::ToolSource); the ToolSearch tool calls
//! [`search_tools`] and [`ToolDeferral::load`].
//!
//! What defers: MCP tools by default (built-ins are few, small and used
//! constantly). A host with many tools of its own defers every tool but a list
//! it always needs ([`ToolDeferral::deferring_all_except`]).
//!
//! A deferred tool the model calls by name, without ToolSearch, still runs and
//! counts as loaded: a prompt that names a tool,
//! or a resumed conversation that called one ([`ToolDeferral::load_called`]),
//! never meets "Unknown tool".
//!
//! The ToolSearch / CallTool tools themselves live in `haest-tools` (`tool_search`).

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, MutexGuard},
};

use crate::{
    messages::Message,
    tool::{Tool, ToolRef, Tools},
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

/// Exact `select:a,b`, else keyword match over name + description, best first (at most 5).
///
/// `select:` returns the named tools in `pool` order, unknown names skipped.
/// A keyword query scores each tool per word: 3 for a hit in the name, 1 for
/// a hit anywhere in name or description, case-insensitive; ties keep `pool`
/// order and tools that score nothing are left out.
pub fn search_tools(pool: &[ToolRef], query: &str) -> Tools {
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
    let mut scored: Vec<(usize, &ToolRef)> = pool
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

/// One agent's deferral state: which deferred tools it has loaded.
///
/// A cheap `Clone`: every clone shares what is loaded, so the host's tool
/// source and the ToolSearch tool that loads them hold the same deferral.
///
/// By default MCP tools (names starting `mcp__`) defer, except those a host
/// keeps loaded ([`with_always_loaded`](Self::with_always_loaded)).
///
/// # Examples
///
/// A host makes one per agent and hands it to haest-tools' `DeferredTools`,
/// with its own tools that every turn needs kept out of deferral:
///
/// ```
/// use haest_core::deferred_tools::{DeferMode, ToolDeferral};
///
/// let deferral = ToolDeferral::new(DeferMode::Grow)
///     .deferring_all_except(["Read", "Bash"]);
/// assert_eq!(deferral.mode(), DeferMode::Grow);
/// // What ToolSearch does when the model asks for a schema; in `grow` the
/// // tool joins the advertised list from the next call on.
/// deferral.load(["mcp__docs__search"]);
/// ```
#[derive(Clone, Debug)]
pub struct ToolDeferral {
    mode: DeferMode,
    loaded: Arc<Mutex<HashSet<String>>>,
    always_loaded: Arc<[String]>,
    /// `Some`: every tool defers but these names. `None`: MCP tools only.
    all_but: Option<Arc<HashSet<String>>>,
}

impl ToolDeferral {
    /// Deferral in `mode`, nothing loaded yet, MCP tools deferring.
    pub fn new(mode: DeferMode) -> Self {
        Self {
            mode,
            loaded: Arc::default(),
            always_loaded: Arc::new([]),
            all_but: None,
        }
    }

    /// Name suffixes of MCP tools that are never deferred: the ones a turn
    /// must not wait on a ToolSearch for (a host's `__send_message`, so a
    /// reply to another conversation goes out at once). None by default.
    #[must_use]
    pub fn with_always_loaded<S: Into<String>>(
        mut self,
        suffixes: impl IntoIterator<Item = S>,
    ) -> Self {
        self.always_loaded = suffixes.into_iter().map(Into::into).collect();
        self
    }

    /// Defer EVERY tool, the host's own too, except these names (the ones
    /// nearly every turn uses). MCP tools matching [`Self::with_always_loaded`] stay too.
    #[must_use]
    pub fn deferring_all_except<S: Into<String>>(
        mut self,
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        self.all_but = Some(Arc::new(names.into_iter().map(Into::into).collect()));
        self
    }

    /// The mode it was made with.
    pub fn mode(&self) -> DeferMode {
        self.mode
    }

    /// Mark tools loaded (ToolSearch did). Only `grow` changes the advertised list because of it.
    pub fn load<S: Into<String>>(&self, names: impl IntoIterator<Item = S>) {
        let names: Vec<String> = names.into_iter().map(Into::into).collect();
        self.loaded().extend(names);
    }

    /// What is loaded. Only this module's code runs under the lock, so a
    /// poisoned one is a bug here.
    pub(crate) fn loaded(&self) -> MutexGuard<'_, HashSet<String>> {
        self.loaded.lock().expect("the deferral's lock poisoned")
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

    /// `all` split into the deferred tools and the rest, in `all` order;
    /// None when nothing defers (`off`, or no tool matches).
    fn split(&self, all: &[ToolRef]) -> Option<(Vec<ToolRef>, Vec<ToolRef>)> {
        if self.mode == DeferMode::Off {
            return None;
        }
        let (deferred, eager): (Vec<_>, Vec<_>) =
            all.iter().cloned().partition(|t| self.is_deferrable(&**t));
        (!deferred.is_empty()).then_some((deferred, eager))
    }
}

pub(crate) mod split;

#[cfg(test)]
mod tests;
