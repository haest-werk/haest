//! [`LiveTools`]: a set of tools a host changes while the conversation runs.

use std::{
    fmt, mem,
    sync::{Arc, Mutex, MutexGuard},
};

use super::{ToolRef, ToolSource, Tools};

/// A set of tools the host changes while a conversation runs: add one,
/// replace one with a new version, take one away for a while, or reload
/// them all. A [`ToolSource`]: hand a clone to the agent
/// ([`AgentDeps::new`](crate::AgentDeps::new)) and keep one to change.
///
/// A cheap `Clone`: every clone is the same set, so a change made through
/// any of them is what every other one reads next. Each method takes
/// `&self`.
///
/// - [`set_tool`](Self::set_tool) adds a tool at the end, or replaces the one
///   of the same name where it stands.
/// - [`remove_tool`](Self::remove_tool) drops one.
/// - [`set_enabled`](Self::set_enabled) takes one out of what the model sees
///   and may call, and puts it back as it was, in its place.
/// - [`set_tools`](Self::set_tools) replaces the whole set (a reload).
///
/// # When the loop sees a change
///
/// The loop reads [`tools`](ToolSource::tools) before each model call, for
/// what that call advertises, and [`callable`](ToolSource::callable) when it
/// runs the calls of the answer, for what they may run. A change made
/// between two model calls is in the next one. A call already running
/// finishes on the tool it started with, even when that tool is replaced or
/// removed meanwhile, and so do the other calls of the same answer: they run
/// on the set as it was read for that answer. A call to a tool that is
/// disabled or gone gets the loop's answer to a name it does not know: an
/// error result naming the tools there are, which the model reads.
///
/// # The prompt cache
///
/// A provider caches the start of a request, and the tools come first in it,
/// ahead of the system prompt and the history. So a replacement the model
/// cannot tell from the old tool (the same name, description and schema, and
/// the same line in the prompt's tool guide, [`Tool::prompt_guide`](super::Tool::prompt_guide))
/// leaves the request's bytes and the cache as they were: that is why a
/// replacement keeps its place. Any other change (a tool added, removed,
/// disabled or enabled, or a new description or schema) changes the request
/// from the tools on, and the next model call pays one cache miss.
///
/// A host that changes its tools' schemas or descriptions often defers those
/// tools ([`DeferMode::Proxy`](crate::deferred_tools::DeferMode::Proxy)): the
/// model is sent only a deferred tool's name and the first sentence of its
/// description (in ToolSearch's), never its schema, so a change to the rest
/// costs no miss. A deferred tool added, removed, disabled or enabled still
/// changes ToolSearch's list of names, and costs the one miss.
///
/// # Examples
///
/// ```
/// use haest_core::{LiveTools, Tool, ToolSource, Tools};
/// # use async_trait::async_trait;
/// # use haest_core::{JsonMap, ToolContext, ToolError, ToolOutput};
/// # use serde_json::{Value, json};
/// /// Looks things up; its version is in its description.
/// struct Lookup(&'static str);
/// # #[async_trait]
/// # impl Tool for Lookup {
/// #     fn name(&self) -> &str { "Lookup" }
/// #     fn description(&self) -> String { format!("Look a word up ({}).", self.0) }
/// #     fn parameters(&self) -> Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "dict.lookup" }
/// #     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
/// #         Ok(ToolOutput::text(self.0))
/// #     }
/// # }
/// /// Tells the time.
/// struct Clock;
/// # #[async_trait]
/// # impl Tool for Clock {
/// #     fn name(&self) -> &str { "Clock" }
/// #     fn description(&self) -> String { "Tell the time.".into() }
/// #     fn parameters(&self) -> Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "time.now" }
/// #     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
/// #         Ok(ToolOutput::text("noon"))
/// #     }
/// # }
///
/// let live = LiveTools::from(Tools::new().with(Clock).with(Lookup("v1")));
/// // The agent's clone, as `AgentDeps::new(shared, live.clone(), prompt)` holds it.
/// let agents = live.clone();
/// let names = |tools: Tools| tools.iter().map(|t| t.name().to_string()).collect::<Vec<_>>();
///
/// // Neither advertised nor callable from the next model call on.
/// assert!(live.set_enabled("Clock", false));
/// assert_eq!(names(agents.tools()), ["Lookup"]);
///
/// // A new version, in the old one's place; Clock comes back where it was.
/// live.set_tool(Lookup("v2"));
/// live.set_enabled("Clock", true);
/// assert_eq!(names(agents.tools()), ["Clock", "Lookup"]);
/// assert_eq!(agents.tools()[1].description(), "Look a word up (v2).");
///
/// assert!(live.remove_tool("Clock"));
/// assert_eq!(live.names(), ["Lookup"]);
/// ```
#[derive(Clone, Default)]
pub struct LiveTools(Arc<Mutex<Set>>);

/// The set, behind its lock. Only this file's code runs under it, never a
/// tool's: names are read before a tool goes in, and what a change takes
/// out is dropped once the lock is released.
#[derive(Default)]
struct Set {
    /// Every tool, in order, the disabled ones included.
    entries: Vec<Entry>,
    /// The enabled ones, in order: what the loop reads, made at each change.
    enabled: Tools,
}

/// One tool of the set.
struct Entry {
    /// Its name, read once, when it was set.
    name: String,
    tool: ToolRef,
    enabled: bool,
}

impl Entry {
    /// `tool`, enabled; its name is read here.
    fn new(tool: ToolRef) -> Self {
        Self {
            name: tool.name().to_string(),
            tool,
            enabled: true,
        }
    }
}

impl LiveTools {
    /// No tools yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `tool` at the end, enabled, or put it in place of the tool of the
    /// same name: in that one's position, enabled or disabled as it was.
    pub fn set_tool(&self, tool: impl Into<ToolRef>) {
        let mut entry = Entry::new(tool.into());
        let _replaced = self.change(|entries| {
            let Some(listed) = entries.iter_mut().find(|e| e.name == entry.name) else {
                entries.push(entry);
                return None;
            };
            entry.enabled = listed.enabled;
            Some(mem::replace(listed, entry))
        });
    }

    /// Drop the tool called `name`; the others keep their order. False when
    /// no tool is called that.
    pub fn remove_tool(&self, name: &str) -> bool {
        let removed = self.change(|entries| {
            let at = entries.iter().position(|e| e.name == name)?;
            Some(entries.remove(at))
        });
        removed.is_some()
    }

    /// Enable or disable the tool called `name`. A disabled tool is neither
    /// advertised nor callable, but keeps its place: enabled again, it is
    /// back as it was, where it was. False when no tool is called that.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> bool {
        self.change(|entries| {
            let Some(listed) = entries.iter_mut().find(|e| e.name == name) else {
                return false;
            };
            listed.enabled = enabled;
            true
        })
    }

    /// Replace the whole set with `tools`, in their order, one per name as a
    /// [`Tools`] list keeps them (a later tool of a name takes an earlier
    /// one's place). A name in the old set too keeps its enabled state; a
    /// name that goes forgets it, so it comes back enabled.
    pub fn set_tools<T: Into<ToolRef>>(&self, tools: impl IntoIterator<Item = T>) {
        let tools: Tools = tools.into_iter().collect();
        let mut fresh: Vec<Entry> = tools.iter().cloned().map(Entry::new).collect();
        let _old = self.change(|entries| {
            for entry in &mut fresh {
                if let Some(had) = entries.iter().find(|e| e.name == entry.name) {
                    entry.enabled = had.enabled;
                }
            }
            mem::replace(entries, fresh)
        });
    }

    /// Every tool's name, in order, the disabled ones included.
    pub fn names(&self) -> Vec<String> {
        self.set().entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Whether the tool called `name` is enabled: advertised and callable.
    /// False when no tool is called that.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.set()
            .entries
            .iter()
            .any(|e| e.name == name && e.enabled)
    }

    /// The set. Only this file's code runs under the lock, so a poisoned one
    /// is a bug here.
    fn set(&self) -> MutexGuard<'_, Set> {
        self.0.lock().expect("the live tools' lock poisoned")
    }

    /// Run `f` on the entries, then make the enabled list again. What `f`
    /// returns, and the list it replaced, are handed back with the set
    /// unlocked: the last handle on a tool taken out runs that tool's drop,
    /// a host's code.
    fn change<R>(&self, f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
        let (out, _before) = {
            let mut set = self.set();
            let out = f(&mut set.entries);
            let enabled = set.entries.iter().filter(|e| e.enabled);
            let enabled = Tools::from_distinct(enabled.map(|e| e.tool.clone()).collect());
            (out, mem::replace(&mut set.enabled, enabled))
        };
        out
    }
}

/// The tools of `tools`, in order, every one enabled.
impl From<Tools> for LiveTools {
    fn from(tools: Tools) -> Self {
        let live = Self::new();
        live.set_tools(tools);
        live
    }
}

/// The names, in order, and which of them are disabled.
impl fmt::Debug for LiveTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (names, disabled): (Vec<String>, Vec<String>) = {
            let set = self.set();
            let names = set.entries.iter().map(|e| e.name.clone()).collect();
            let off = set.entries.iter().filter(|e| !e.enabled);
            (names, off.map(|e| e.name.clone()).collect())
        };
        f.debug_struct("LiveTools")
            .field("names", &names)
            .field("disabled", &disabled)
            .finish_non_exhaustive()
    }
}

/// The enabled tools, in order. Nothing is kept back: a disabled tool is not
/// callable either.
impl ToolSource for LiveTools {
    fn tools(&self) -> Tools {
        self.set().enabled.clone()
    }
    fn callable(&self) -> Tools {
        self.tools()
    }
}
