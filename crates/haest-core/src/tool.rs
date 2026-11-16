//! The tool contract every haest tool implements.
//!
//! Tools are provider-neutral: the parameter schema is plain JSON Schema, and a
//! tool reports both what the MODEL sees (`content`) and an optional canonical
//! result envelope a host renders (`result`). Each tool states its canonical
//! `kind` itself ("file.read", "shell.exec", ...), so no host has to guess.
//!
//! PLUG POINT: implement [`Tool`] to add a tool; implement [`ToolHost`] to receive
//! what tools publish, report and log. A [`ToolSource`] (a plain
//! `Vec<Arc<dyn Tool>>` is one) hands an agent its tools through
//! [`AgentDeps::tools`](crate::AgentDeps::tools).
//!
//! The loop gives every call a [`ToolContext`] (working directory, cancel
//! token, workspace, host) and turns whatever comes back, an error or a panic
//! included, into exactly one result in history
//! ([`tool_exec`](crate::tool_exec)). The `req_*` / `opt_*` helpers read
//! arguments the way models actually send them.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::{
    fs::resolve_path,
    messages::{MediaPart, ToolCall},
    workspace::Workspace,
};

/// A tool call's arguments: a JSON object.
pub type JsonMap = Map<String, Value>;

/// The built-in tools that work on files by path (haest-tools'). The prompt's
/// path line and its workspace note go by them; a Code run may always call them.
pub const FILE_TOOLS: [&str; 5] = ["Read", "Write", "Edit", "Grep", "Glob"];

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
    /// Spend an agent the tool ran already charged itself (a sub-agent's model
    /// calls): added to the turn's cost so the turn cap sees it, never charged twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_cost_usd: Option<f64>,
    /// The tool already fit `content` to its own budget: the generic spill must not touch it.
    /// Otherwise `content` over [`MAX_TOOL_OUTPUT`](crate::tool_exec::MAX_TOOL_OUTPUT)
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
/// Reported through [`ToolHost::on_inner_call`]: a `Start`, then an `End`
/// with the same call.
#[derive(Debug, Clone)]
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
/// Every [`AgentHooks`](crate::AgentHooks) is one; a tool reaches it through
/// [`ToolContext::host`]. [`NoHost`] offers nothing.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Put bytes where the host's transcript can show them; the URL, or None.
    async fn publish(&self, _data: Vec<u8>, _media_type: &str) -> Option<String> {
        None
    }
    /// A tool that runs OTHER tools (Code) reports each one here, so the human sees it.
    fn on_inner_call(&self, _ev: InnerCallEvent) {}
    /// haest's diagnostic log.
    fn log(&self, _message: &str) {}
}

/// A host that offers nothing.
#[derive(Debug, Default)]
pub struct NoHost;
impl ToolHost for NoHost {}

/// What one tool call may use: where it runs, how it learns it was cancelled,
/// the workspace and the host.
///
/// The loop builds one per call from [`AgentDeps`](crate::AgentDeps);
/// [`ToolContext::new`] builds one to run a tool outside the loop.
#[derive(Clone)]
pub struct ToolContext {
    /// The working directory; [`ToolContext::resolve`] takes relative paths from it.
    pub cwd: PathBuf,
    /// The turn's cancel token. It fires on abort; a tool that runs for long
    /// should stop when it does.
    pub cancel: CancellationToken,
    /// The only way a tool touches files or runs a command.
    pub workspace: Arc<dyn Workspace>,
    /// The host, for publishing bytes, reporting inner calls and logging.
    pub host: Arc<dyn ToolHost>,
    /// Tokens a result may still add before the next model call hits the request
    /// cap; None when no cap is configured.
    pub context_room_tokens: Option<u64>,
    /// This call's id; None when a tool is run outside the loop.
    pub tool_call_id: Option<String>,
    /// Where results too big for the context are written.
    pub spill_dir: Option<PathBuf>,
}

impl ToolContext {
    /// A context for running a tool outside the loop (tests, a host command):
    /// a fresh cancel token, [`NoHost`], no context cap, no call id, no spill directory.
    pub fn new(cwd: impl Into<PathBuf>, workspace: Arc<dyn Workspace>) -> Self {
        Self {
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            workspace,
            host: Arc::new(NoHost),
            context_room_tokens: None,
            tool_call_id: None,
            spill_dir: None,
        }
    }

    /// A path the model gave, made absolute: a relative one is taken from
    /// `cwd`, and `.` / `..` are resolved without touching any file system.
    pub fn resolve(&self, p: &str) -> PathBuf {
        resolve_path(&self.cwd, p)
    }
}

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
/// use std::sync::Arc;
///
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
/// # async fn main() {
/// // Outside the loop: a context of its own. The loop builds one per call.
/// let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
/// let input = json!({"a": 2, "b": "3"}); // models send numbers as strings too
/// let out = Add.execute(input.as_object().unwrap().clone(), &ctx).await.unwrap();
/// assert_eq!(out.content, "5");
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
    /// time ([`tool_batch`](crate::tool_batch)); any other call runs alone.
    fn read_only(&self) -> bool {
        false
    }
    /// Canonical input shape for a host's transcript. Defaults to the raw input.
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        input.clone()
    }
    /// Run one call with the model's arguments.
    ///
    /// The model reads the returned [`ToolOutput`]. A result longer than
    /// [`MAX_TOOL_OUTPUT`](crate::tool_exec::MAX_TOOL_OUTPUT) characters goes
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

/// Where the loop gets its tools, asked again before every model call (deferred
/// tools and MCP servers change the list mid-conversation).
///
/// A `Vec<Arc<dyn Tool>>` is a fixed list, every tool advertised.
pub trait ToolSource: Send + Sync {
    /// The tools the next model call advertises.
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    /// The tools a call may run: the advertised ones, plus any the source keeps
    /// back (a deferred tool the model calls by name runs instead of failing).
    fn callable(&self) -> Vec<Arc<dyn Tool>> {
        self.tools()
    }
}

impl ToolSource for Vec<Arc<dyn Tool>> {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.clone()
    }
}

/// A required, non-empty string argument.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` is missing, empty or not a string.
///
/// ```
/// use haest_core::tool::{opt_bool, opt_num, opt_str, req_str};
///
/// let input = serde_json::json!({"path": "a.txt", "limit": "20", "all": "true", "glob": ""});
/// let input = input.as_object().unwrap();
/// assert_eq!(req_str(input, "path").unwrap(), "a.txt");
/// assert_eq!(opt_num(input, "limit").unwrap(), Some(20.0));
/// assert!(opt_bool(input, "all"));
/// assert_eq!(opt_str(input, "glob").unwrap(), None); // "" counts as left out
/// assert!(req_str(input, "missing").is_err());
/// ```
pub fn req_str<'a>(input: &'a JsonMap, key: &str) -> Result<&'a str, ToolError> {
    match input.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s),
        _ => Err(ToolError::Input(format!(
            "\"{key}\" is required and must be a string"
        ))),
    }
}

/// An optional string argument; missing, `null` and `""` are all None.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` holds something other than a string.
pub fn opt_str<'a>(input: &'a JsonMap, key: &str) -> Result<Option<&'a str>, ToolError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ToolError::Input(format!("\"{key}\" must be a string"))),
    }
}

/// A number, also as a numeric string (models send both).
/// Missing, `null`, `""` and whitespace are None.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` holds anything else, or a number that is
/// not finite.
pub fn opt_num(input: &JsonMap, key: &str) -> Result<Option<f64>, ToolError> {
    let n = match input.get(key) {
        None | Some(Value::Null) => return Ok(None),
        // Models send "" for a number they mean to leave out.
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(None),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(_) => None,
    };
    n.filter(|f| f.is_finite())
        .map(Some)
        .ok_or_else(|| ToolError::Input(format!("\"{key}\" must be a number")))
}

/// `true` or `"true"`; anything else is false.
pub fn opt_bool(input: &JsonMap, key: &str) -> bool {
    match input.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

/// A value as a reader spells it: a string as itself, anything else as JSON (`16:9`, `80`, `true`).
pub fn spelled(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Keep the head and tail of long output; the middle is where nothing useful
/// lives. `max` counts characters, never splitting one.
///
/// Returns the text and whether it was cut. A cut text keeps `max / 2`
/// characters at each end around a line saying how many were dropped, so it
/// is a little longer than `max`.
///
/// ```
/// use haest_core::tool::truncate_middle;
///
/// let (short, cut) = truncate_middle("abc", 10);
/// assert_eq!((short.as_str(), cut), ("abc", false));
///
/// let (long, cut) = truncate_middle(&"x".repeat(100), 10);
/// assert!(cut && long.starts_with("xxxxx") && long.ends_with("xxxxx"));
/// ```
pub fn truncate_middle(text: &str, max: usize) -> (String, bool) {
    let len = text.chars().count();
    if len <= max {
        return (text.to_string(), false);
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(len - half).collect();
    (
        format!(
            "{head}\n\n... [{} characters truncated] ...\n\n{tail}",
            len - max
        ),
        true,
    )
}

/// The default `truncate_middle` budget, in characters.
pub const TRUNCATE_CHARS: usize = 30_000;

#[cfg(test)]
mod tests;
