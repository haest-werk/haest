//! What one agent's [`ToolDeferral`] advertises and lets a call run, over the
//! tools its source has. haest-tools' `DeferredTools` is the one caller; it
//! reaches these through `haest_core::__private`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{DeferMode, ToolDeferral};
use crate::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput};

/// The tools this call advertises, given every tool the agent has.
///
/// The tools that do not defer, in `all` order, then what `deferral_tools`
/// builds from the deferred list (ToolSearch, and CallTool for `proxy`), then
/// (in `grow`) the loaded deferred ones. With nothing deferring this is `all`,
/// and `deferral_tools` is not called.
pub fn advertised_tools(
    deferral: &ToolDeferral,
    all: &[Arc<dyn Tool>],
    deferral_tools: impl FnOnce(&[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>>,
) -> Vec<Arc<dyn Tool>> {
    let Some((deferred, eager)) = deferral.split(all) else {
        return all.to_vec();
    };
    let grown: Vec<Arc<dyn Tool>> = if deferral.mode == DeferMode::Grow {
        let loaded = deferral.loaded.lock().unwrap();
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

/// Every tool a call may run: the advertised ones and every deferred one,
/// which loads itself when the model calls it by name. With nothing
/// deferring this is `all`, and `deferral_tools` is not called.
pub fn callable_tools(
    deferral: &Arc<ToolDeferral>,
    all: &[Arc<dyn Tool>],
    deferral_tools: impl FnOnce(&[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>>,
) -> Vec<Arc<dyn Tool>> {
    let Some((deferred, mut out)) = deferral.split(all) else {
        return all.to_vec();
    };
    out.extend(deferral_tools(&deferred));
    out.extend(deferred.into_iter().map(|tool| {
        Arc::new(Loading {
            tool,
            deferral: deferral.clone(),
        }) as Arc<dyn Tool>
    }));
    out
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
