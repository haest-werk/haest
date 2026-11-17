//! [`Tools`]: a list of tools, cheap to clone.

use std::{ops::Deref, slice, sync::Arc, vec};

use super::{ToolRef, ToolSource};

/// A list of tools, in order, one per name: what a [`ToolSource`] hands the
/// loop, and the simplest source there is (every tool advertised, every call).
///
/// Cheap to clone (the clones share one list until one of them changes).
/// Built from tools themselves, from [`ToolRef`]s, or from `Arc`s of tools;
/// it derefs to `[ToolRef]` for reading, and [`get`](Self::get) finds one by
/// name. A tool added under a name the list already holds takes that tool's
/// place, so `builtin_tools(..).with(MyRead)` overrides the built-in Read.
/// [`Tools::new`] is the empty list: an agent that only talks.
///
/// # Examples
///
/// ```
/// use async_trait::async_trait;
/// use haest_core::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, ToolSource, Tools};
/// use serde_json::{Value, json};
///
/// struct Ping;
///
/// #[async_trait]
/// impl Tool for Ping {
///     fn name(&self) -> &str {
///         "Ping"
///     }
///     fn description(&self) -> String {
///         "Answers pong.".into()
///     }
///     fn parameters(&self) -> Value {
///         json!({"type": "object"})
///     }
///     fn kind(&self) -> &str {
///         "net.ping"
///     }
///     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
///         Ok(ToolOutput::text("pong"))
///     }
/// }
///
/// let tools = Tools::new().with(Ping);
/// assert_eq!(tools.len(), 1);
/// assert!(tools.tools().contains("Ping"));
/// ```
#[derive(Clone, Default, Debug)]
pub struct Tools(Arc<Vec<ToolRef>>);

impl Tools {
    /// No tools.
    pub fn new() -> Self {
        Self::default()
    }

    /// This list with `tool` added, as [`push`](Self::push) adds it.
    #[must_use]
    pub fn with(mut self, tool: impl Into<ToolRef>) -> Self {
        self.push(tool);
        self
    }

    /// Add `tool` at the end, or in place of the tool of the same name: a
    /// list holds one tool per name, so a host overrides a built-in by adding
    /// its own.
    pub fn push(&mut self, tool: impl Into<ToolRef>) {
        let tool = tool.into();
        let list = Arc::make_mut(&mut self.0);
        match list.iter_mut().find(|t| t.name() == tool.name()) {
            Some(listed) => *listed = tool,
            None => list.push(tool),
        }
    }

    /// This list with every tool of `tools` added, in order, as
    /// [`with`](Self::with) adds one.
    #[must_use]
    pub fn with_all<T: Into<ToolRef>>(mut self, tools: impl IntoIterator<Item = T>) -> Self {
        self.extend(tools);
        self
    }

    /// The tool called `name`.
    pub fn get(&self, name: &str) -> Option<&ToolRef> {
        self.0.iter().find(|t| t.name() == name)
    }

    /// Whether a tool is called `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Take the tool called `name` out of the list; the others keep their
    /// order. None when no tool is called that.
    pub fn remove(&mut self, name: &str) -> Option<ToolRef> {
        let at = self.0.iter().position(|t| t.name() == name)?;
        Some(Arc::make_mut(&mut self.0).remove(at))
    }
}

impl Deref for Tools {
    type Target = [ToolRef];

    fn deref(&self) -> &[ToolRef] {
        &self.0
    }
}

/// One tool per name, as [`Tools::push`] adds them.
impl From<Vec<ToolRef>> for Tools {
    fn from(tools: Vec<ToolRef>) -> Self {
        tools.into_iter().collect()
    }
}

/// One tool per name, as [`Tools::push`] adds them.
impl<T: Into<ToolRef>> FromIterator<T> for Tools {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut tools = Self::new();
        tools.extend(iter);
        tools
    }
}

/// Each tool as [`Tools::push`] adds it.
impl<T: Into<ToolRef>> Extend<T> for Tools {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for tool in iter {
            self.push(tool);
        }
    }
}

impl IntoIterator for Tools {
    type Item = ToolRef;
    type IntoIter = vec::IntoIter<ToolRef>;

    fn into_iter(self) -> Self::IntoIter {
        Arc::unwrap_or_clone(self.0).into_iter()
    }
}

impl<'a> IntoIterator for &'a Tools {
    type Item = &'a ToolRef;
    type IntoIter = slice::Iter<'a, ToolRef>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// A fixed list: every tool advertised, every call.
impl ToolSource for Tools {
    fn tools(&self) -> Tools {
        self.clone()
    }
}
