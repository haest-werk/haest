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
//! Built-in tools are never deferred: they are few, small and used constantly.
//! The ToolSearch / CallTool tools themselves live in `kern-tools` (`tool_search`).

use std::{collections::HashSet, fmt, str::FromStr, sync::Arc, sync::Mutex};

use serde::{Deserialize, Serialize};

use crate::tool::Tool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeferMode {
    Off,
    Proxy,
    Grow,
}

impl DeferMode {
    pub const ALL: [DeferMode; 3] = [DeferMode::Off, DeferMode::Proxy, DeferMode::Grow];

    pub fn as_str(self) -> &'static str {
        match self {
            DeferMode::Off => "off",
            DeferMode::Proxy => "proxy",
            DeferMode::Grow => "grow",
        }
    }
}

impl fmt::Display for DeferMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DeferMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DeferMode::ALL
            .into_iter()
            .find(|m| m.as_str() == s)
            .ok_or_else(|| format!("unknown deferred-tools mode \"{s}\" (off, proxy, grow)"))
    }
}

const MAX_MATCHES: usize = 5;

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
}

impl ToolDeferral {
    pub fn new(mode: DeferMode) -> Self {
        Self {
            mode,
            loaded: Mutex::default(),
            always_loaded: DEFAULT_ALWAYS_LOADED.map(String::from).to_vec(),
        }
    }

    /// Name suffixes of MCP tools that are never deferred (replaces the default).
    pub fn with_always_loaded(mut self, suffixes: impl IntoIterator<Item = String>) -> Self {
        self.always_loaded = suffixes.into_iter().collect();
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

    fn is_deferrable(&self, t: &dyn Tool) -> bool {
        let name = t.name();
        name.starts_with("mcp__")
            && !self
                .always_loaded
                .iter()
                .any(|s| name.ends_with(s.as_str()))
    }

    /// The tools this call advertises, given every tool the agent has and the deferral tools to add.
    pub fn advertise(
        &self,
        all: &[Arc<dyn Tool>],
        deferral_tools: impl FnOnce(&[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>>,
    ) -> Vec<Arc<dyn Tool>> {
        if self.mode == DeferMode::Off {
            return all.to_vec();
        }
        let (deferred, eager): (Vec<_>, Vec<_>) = all
            .iter()
            .cloned()
            .partition(|t| self.is_deferrable(t.as_ref()));
        if deferred.is_empty() {
            return all.to_vec();
        }
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

#[cfg(test)]
mod tests;
