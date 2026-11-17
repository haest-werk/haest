//! [`Tools`]: a list of tools, cheap to clone.

use std::{ops::Deref, slice, sync::Arc, vec};

use super::{ToolRef, ToolSource};

/// A list of tools, in order: what a [`ToolSource`] hands the loop, and the
/// simplest source there is (every tool advertised, every call).
///
/// Cheap to clone (the clones share one list until one of them changes).
/// Built from tools themselves, from [`ToolRef`]s, or from `Arc`s of tools;
/// it derefs to `[ToolRef]` for reading. [`Tools::new`] is the empty list:
/// an agent that only talks.
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
/// assert_eq!(tools.tools()[0].name(), "Ping");
/// ```
#[derive(Clone, Default, Debug)]
pub struct Tools(Arc<Vec<ToolRef>>);

impl Tools {
    /// No tools.
    pub fn new() -> Self {
        Self::default()
    }

    /// This list with `tool` added at the end. If two tools share a name,
    /// the one listed later runs.
    #[must_use]
    pub fn with(mut self, tool: impl Into<ToolRef>) -> Self {
        self.push(tool);
        self
    }

    /// Add `tool` at the end.
    pub fn push(&mut self, tool: impl Into<ToolRef>) {
        Arc::make_mut(&mut self.0).push(tool.into());
    }
}

impl Deref for Tools {
    type Target = [ToolRef];

    fn deref(&self) -> &[ToolRef] {
        &self.0
    }
}

impl From<Vec<ToolRef>> for Tools {
    fn from(tools: Vec<ToolRef>) -> Self {
        Self(Arc::new(tools))
    }
}

impl<T: Into<ToolRef>> FromIterator<T> for Tools {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(Arc::new(iter.into_iter().map(Into::into).collect()))
    }
}

impl<T: Into<ToolRef>> Extend<T> for Tools {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        Arc::make_mut(&mut self.0).extend(iter.into_iter().map(Into::into));
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
