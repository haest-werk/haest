//! The tool contract every kern tool implements.
//!
//! Tools are provider-neutral: the parameter schema is plain JSON Schema, and a
//! tool reports both what the MODEL sees (`content`) and an optional canonical
//! result envelope a host renders (`result`). Each tool states its canonical
//! `kind` itself ("file.read", "shell.exec", ...), so no host has to guess.
//!
//! PLUG POINT: implement [`Tool`] to add a tool; implement [`ToolHost`] to receive
//! what tools publish, report and log.

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

pub type JsonMap = Map<String, Value>;

/// The built-in tools that work on files by path (kern-tools'). The prompt's
/// path line and its workspace note go by them; a Code run may always call them.
pub const FILE_TOOLS: [&str; 5] = ["Read", "Write", "Edit", "Grep", "Glob"];

/// What a tool's run produced.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    /// What the model receives.
    pub content: String,
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
    #[serde(default)]
    pub sized: bool,
    /// The finished call's title for a host's display, when only the outcome can
    /// tell ("Notified you: X", "Took you to X"); None = the host's own title for
    /// the call. The model never sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            ..Default::default()
        }
    }
    pub fn with_result(mut self, result: Value) -> Self {
        self.result = Some(result);
        self
    }
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// Why a tool call failed.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// A bad call the model can correct; the message goes back verbatim.
    #[error("{0}")]
    Input(String),
    /// Anything else; the message goes back as an error result.
    #[error("{0}")]
    Failed(String),
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        ToolError::Failed(e.to_string())
    }
}

/// A tool call made by another tool (call `parent_id`): a transcript row of its own, not a model turn.
#[derive(Debug, Clone)]
pub enum InnerCallEvent {
    Start {
        parent_id: String,
        call: ToolCall,
    },
    End {
        parent_id: String,
        call: ToolCall,
        output: ToolOutput,
        duration_ms: u64,
    },
}

/// What the host offers every tool. All optional: the defaults do nothing.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Put bytes where the host's transcript can show them; the URL, or None.
    async fn publish(&self, _data: Vec<u8>, _media_type: &str) -> Option<String> {
        None
    }
    /// A tool that runs OTHER tools (Code) reports each one here, so the human sees it.
    fn on_inner_call(&self, _ev: InnerCallEvent) {}
    /// kern's diagnostic log.
    fn log(&self, _message: &str) {}
}

/// A host that offers nothing.
#[derive(Debug, Default)]
pub struct NoHost;
impl ToolHost for NoHost {}

#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    /// The only way a tool touches files or runs a command.
    pub workspace: Arc<dyn Workspace>,
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
    /// A context for running a tool outside the loop (tests, a host command).
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

    pub fn resolve(&self, p: &str) -> PathBuf {
        resolve_path(&self.cwd, p)
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> String;
    /// JSON Schema of the input object.
    fn parameters(&self) -> Value;
    /// Canonical kind, e.g. "file.read" or "mcp.server.tool".
    fn kind(&self) -> &str;
    /// Touches nothing another call could see (no writes, no side effects): may run in parallel.
    fn read_only(&self) -> bool {
        false
    }
    /// Canonical input shape for a host's transcript. Defaults to the raw input.
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        input.clone()
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError>;
}

/// Where the loop gets its tools, asked again before every model call (deferred
/// tools and MCP servers change the list mid-conversation).
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

pub fn req_str<'a>(input: &'a JsonMap, key: &str) -> Result<&'a str, ToolError> {
    match input.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s),
        _ => Err(ToolError::Input(format!(
            "\"{key}\" is required and must be a string"
        ))),
    }
}

pub fn opt_str<'a>(input: &'a JsonMap, key: &str) -> Result<Option<&'a str>, ToolError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ToolError::Input(format!("\"{key}\" must be a string"))),
    }
}

/// A number, also as a numeric string (models send both).
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

/// Keep the head and tail of long output; the middle is where nothing useful
/// lives. `max` counts characters, never splitting one.
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
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> JsonMap {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn input_helpers_accept_what_models_send() {
        let i = map(json!({"a": "x", "n": "42", "b": "true", "e": "", "blank": "  "}));
        assert_eq!(req_str(&i, "a").unwrap(), "x");
        assert!(req_str(&i, "e").is_err());
        assert_eq!(opt_str(&i, "e").unwrap(), None);
        assert_eq!(opt_num(&i, "n").unwrap(), Some(42.0));
        // "" for a number is no number.
        assert_eq!(opt_num(&i, "e").unwrap(), None);
        assert_eq!(opt_num(&i, "blank").unwrap(), None);
        assert!(opt_bool(&i, "b"));
        assert!(opt_num(&map(json!({"n": "x"})), "n").is_err());
    }

    #[test]
    fn truncate_middle_keeps_both_ends() {
        let (t, cut) = truncate_middle("abcdefghij", 4);
        assert!(cut);
        assert!(t.starts_with("ab") && t.ends_with("ij") && t.contains("6 characters truncated"));
        assert_eq!(truncate_middle("short", 10), ("short".to_string(), false));
    }

    #[test]
    fn a_title_is_the_hosts_and_absent_unless_set() {
        let plain = serde_json::to_value(ToolOutput::text("sent")).unwrap();
        assert!(plain.get("title").is_none());
        let titled = ToolOutput::text("sent").with_title("Notified you: done");
        assert_eq!(
            serde_json::to_value(&titled).unwrap()["title"],
            "Notified you: done"
        );
        assert_eq!(titled.content, "sent");
    }
}
