//! The tool contract every haest tool implements.
//!
//! Tools are provider-neutral: the parameter schema is plain JSON Schema, and a
//! tool reports both what the MODEL sees (`content`) and an optional canonical
//! result envelope a host renders (`result`). Each tool states its canonical
//! `kind` itself ("file.read", "shell.exec", ...), so no host has to guess.
//!
//! PLUG POINT: implement [`Tool`] to add a tool; implement [`ToolServices`] to receive
//! what tools publish, report and log. A [`ToolSource`] (a [`Tools`] list is
//! one) hands an agent its tools through
//! [`AgentDeps::tools`](crate::AgentDeps::tools). The loop holds each tool
//! by a [`ToolRef`].
//!
//! The loop gives every call a [`ToolContext`] (working directory, cancel
//! token, workspace, the host's services) and turns whatever comes back, an
//! error or a panic included, into exactly one result in history. The
//! `req_*` / `opt_*` helpers read arguments the way models actually send them,
//! and [`slug`] makes a name (a file's, an id) from free text.

mod args;
mod context;
mod list;
mod resume;
#[cfg(test)]
mod tests;
mod truncate;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use args::{opt_bool, opt_num, opt_str, req_str, spelled};
pub use context::ToolContext;
pub use list::Tools;
pub use resume::OnResume;
pub use truncate::{TRUNCATE_CHARS, truncate_middle};

pub use crate::slug::slug;

use crate::messages::{MediaPart, ToolCall};

/// A tool call's arguments: a JSON object.
pub type JsonMap = Map<String, Value>;

/// What a tool's run produced.
///
/// The model reads `content` (and sees `media`); everything else is for the
/// host and the loop's accounting. [`ToolOutput::text`] and
/// [`ToolOutput::error`] cover most tools.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    /// What the model receives.
    pub content: String,
    /// The call failed; `content` says why to the model.
    #[serde(default)]
    pub is_error: bool,
    /// Canonical envelope for a host's transcript (`{"kind": "text" | "shell" | "file" | ...}`);
    /// None = a text envelope of `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Images / PDFs the model should SEE, not read about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaPart>,
    /// What the tool itself spent (an image model's bill): added to the turn's cost AND charged to the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Spend an agent the tool ran already charged itself (a subagent's model
    /// calls): added to the turn's cost so the turn cap sees it, never charged twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_cost_usd: Option<f64>,
    /// The tool already fit `content` to its own budget: the generic spill must not touch it.
    /// Otherwise `content` over [`MAX_TOOL_OUTPUT`]
    /// characters goes to a file and the model gets a preview.
    #[serde(default)]
    pub sized: bool,
    /// The finished call's title for a host's display, when only the outcome can
    /// tell ("Notified you: X", "Took you to X"); None = the host's own title for
    /// the call. The model never sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl ToolOutput {
    /// A successful result: `content` for the model, nothing else.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Default::default()
        }
    }
    /// A failed result: `content` tells the model what went wrong.
    ///
    /// The model reads `content` as is, as it would a [`ToolError::Input`].
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            ..Default::default()
        }
    }
    /// The same output with `result` as the host's envelope.
    pub fn with_result(mut self, result: Value) -> Self {
        self.result = Some(result);
        self
    }
    /// The same output with `title` as the host's title for the finished call.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// Why a tool call failed. [`Tool::execute`] returns it; the loop turns it
/// into an error result the model reads, and the turn goes on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// A bad call the model can correct; the message goes back verbatim.
    #[error("{0}")]
    Input(String),
    /// Anything else; the message goes back as an error result, prefixed with
    /// the tool's name (`Read failed: ...`).
    #[error("{0}")]
    Failed(String),
}

/// An I/O error is a [`ToolError::Failed`] with the error's text.
impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        ToolError::Failed(e.to_string())
    }
}

/// A tool call made by another tool (call `parent_id`): a transcript row of its own, not a model turn.
///
/// Reported through [`ToolServices::on_inner_call`]: a `Start`, then an `End`
/// with the same call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum InnerCallEvent {
    /// The inner call is about to run.
    Start {
        /// The id of the tool call that made it.
        parent_id: String,
        /// The inner call.
        call: ToolCall,
    },
    /// The inner call finished.
    End {
        /// The id of the tool call that made it.
        parent_id: String,
        /// The inner call.
        call: ToolCall,
        /// What it produced.
        output: ToolOutput,
        /// How long it ran.
        duration_ms: u64,
    },
}

/// What the host offers every tool. All optional: the defaults do nothing.
///
/// Set it with [`SharedDeps::with_services`](crate::SharedDeps::with_services);
/// a tool reaches it through [`ToolContext::services`]. [`NoServices`]
/// offers nothing.
#[async_trait]
pub trait ToolServices: Send + Sync {
    /// Put bytes where the host's transcript can show them; the URL, or None.
    async fn publish(&self, _data: Vec<u8>, _media_type: &str) -> Option<String> {
        None
    }
    /// A tool that runs OTHER tools (Code) reports each one here, so the human sees it.
    fn on_inner_call(&self, _ev: InnerCallEvent) {}
    /// A line for haest's diagnostic log (what it does under the hood),
    /// which the human need not see. Default: dropped.
    fn debug(&self, _message: &str) {}
}

crate::plug_ref! {
    /// A [`ToolServices`], shared: what [`SharedDeps::services`](crate::SharedDeps::services)
    /// and every [`ToolContext`] hold.
    pub struct ToolServicesRef(dyn ToolServices);
}

/// Services that offer nothing: publish returns no URL, the rest is dropped.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoServices;

impl ToolServices for NoServices {}

/// A tool the model can call.
///
/// Implement it with `#[async_trait]` (from the `async-trait` crate) and hand
/// the tool to an agent through a [`ToolSource`]. The model sees `name`,
/// `description` and `parameters`; the host sees `kind` and
/// `canonical_input`.
///
/// # Examples
///
/// ```
/// use async_trait::async_trait;
/// use haest_core::{
///     ShutWorkspace, Tool, ToolContext, ToolError, ToolOutput,
///     tool::{JsonMap, opt_num},
/// };
/// use serde_json::{Value, json};
///
/// /// Adds two numbers.
/// struct Add;
///
/// #[async_trait]
/// impl Tool for Add {
///     fn name(&self) -> &str {
///         "Add"
///     }
///     fn description(&self) -> String {
///         "Add two numbers.".into()
///     }
///     fn parameters(&self) -> Value {
///         json!({
///             "type": "object",
///             "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
///             "required": ["a", "b"]
///         })
///     }
///     fn kind(&self) -> &str {
///         "math.add"
///     }
///     fn read_only(&self) -> bool {
///         true // it changes nothing, so calls may run in parallel
///     }
///     async fn execute(&self, input: JsonMap, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
///         let missing = |key: &str| ToolError::Input(format!("\"{key}\" is required"));
///         let a = opt_num(&input, "a")?.ok_or_else(|| missing("a"))?;
///         let b = opt_num(&input, "b")?.ok_or_else(|| missing("b"))?;
///         Ok(ToolOutput::text((a + b).to_string()))
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Outside the loop: a context of its own. The loop builds one per call.
/// let ctx = ToolContext::new("/", ShutWorkspace);
/// let input: JsonMap = serde_json::from_value(json!({"a": 2, "b": "3"}))?; // models send numbers as strings too
/// let out = Add.execute(input, &ctx).await?;
/// assert_eq!(out.content, "5");
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls it by. If two of an agent's tools share a
    /// name, the one listed later runs.
    fn name(&self) -> &str;
    /// What the tool does and when to use it, for the model.
    fn description(&self) -> String;
    /// JSON Schema of the input object.
    fn parameters(&self) -> Value;
    /// Canonical kind, e.g. "file.read" or "mcp.server.tool".
    fn kind(&self) -> &str;
    /// Touches nothing another call could see (no writes, no side effects): may run in parallel.
    ///
    /// Consecutive read-only calls of one model response run at the same
    /// time; any other call runs alone.
    fn read_only(&self) -> bool {
        false
    }
    /// What a resumed durable run ([`crate::run`]) does with a call of this
    /// tool that may have run before the stop: it started, and its result
    /// never landed.
    ///
    /// The default: a read-only tool runs again ([`OnResume::Rerun`]), any
    /// other is answered as interrupted ([`OnResume::Report`]). Only durable
    /// runs ask; a plain turn never does.
    fn on_resume(&self) -> OnResume {
        if self.read_only() {
            OnResume::Rerun
        } else {
            OnResume::Report
        }
    }
    /// Canonical input shape for a host's transcript. Defaults to the raw input.
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        input.clone()
    }
    /// Run one call with the model's arguments.
    ///
    /// The model reads the returned [`ToolOutput`]. A result longer than
    /// [`MAX_TOOL_OUTPUT`] characters goes
    /// to a file and the model gets a preview, unless [`ToolOutput::sized`] is set.
    ///
    /// # Errors
    ///
    /// [`ToolError::Input`] for a call the model can fix (its message goes
    /// back verbatim), [`ToolError::Failed`] for anything else. Either way the
    /// model gets an error result and the turn goes on; a panic is answered
    /// the same way, as a failure.
    ///
    /// # Cancellation
    ///
    /// The loop does not interrupt a running call: a tool that runs for long
    /// watches [`ToolContext::cancel`] and returns when it fires.
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError>;
}

crate::plug_ref! {
    /// A [`Tool`], shared: how a [`Tools`] list and the loop hold one.
    /// Built from the tool itself, or from an `Arc` of one.
    pub struct ToolRef(dyn Tool);
    debug(tool, f) {
        f.debug_tuple("ToolRef").field(&tool.name()).finish()
    }
}

/// Where the loop gets its tools, asked again before every model call (deferred
/// tools and MCP servers change the list mid-conversation).
///
/// A [`Tools`] list is a fixed one, every tool advertised.
pub trait ToolSource: Send + Sync {
    /// The tools the next model call advertises.
    fn tools(&self) -> Tools;
    /// The tools a call may run: the advertised ones, plus any the source keeps
    /// back (a deferred tool the model calls by name runs instead of failing).
    fn callable(&self) -> Tools {
        self.tools()
    }
}

crate::plug_ref! {
    /// A [`ToolSource`], shared: what [`AgentDeps::tools`](crate::AgentDeps::tools) holds.
    pub struct ToolSourceRef(dyn ToolSource);
}

/// The most any one tool result may put into the model's context (~5k
/// tokens), in characters. A longer result goes to a file and the model gets
/// a preview, unless the tool set [`ToolOutput::sized`].
pub const MAX_TOOL_OUTPUT: usize = 20_000;
