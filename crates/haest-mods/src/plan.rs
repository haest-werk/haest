//! PLAN MODE (built-in mod): `/plan [what to plan]` -- or a host's plan toggle,
//! which runs the same command -- puts haest in read-only planning until
//! `/plan off`. Two halves, because instructions alone are only a request:
//!   - the system prompt gains the planning brief (what a plan skill would say);
//!   - every tool that is not read-only is refused, except the planning tools
//!     themselves (by default the task list, subagents, ToolSearch). Subagents
//!     run through the same mods, so they are read-only too.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use haest_core::{error::BoxError, tool::ToolContext};

use crate::api::{Before, Mod, ModEnv, ModToolCall};

/// The slash command that drives plan mode, without its `/`: `/plan [topic]`
/// turns it on, `/plan off` turns it off.
pub const PLAN_COMMAND: &str = "plan";

/// The tools plan mode lets run though they are not read-only, unless a host
/// names others ([`PlanMod::with_planning_tools`],
/// [`ModConfig::planning_tools`](crate::ModConfig::planning_tools)): haest's
/// task list, subagents and `ToolSearch`.
pub const PLANNING_TOOLS: &[&str] = &[
    "TaskCreate",
    "TaskUpdate",
    "TaskList",
    "TaskGet",
    "Agent",
    "SendMessage",
    "StopAgent",
    "ToolSearch",
];

/// The planning brief plan mode appends to the system prompt while it is on:
/// explore read-only, answer with a plan, record the steps as tasks, change
/// nothing.
pub const PLAN_PROMPT: &str = "# Plan mode is ON\nThe human wants a plan before any change. Explore with read-only tools (Read, Grep, Glob, WebFetch, subagents), then answer with the plan: the goal in a sentence, the files you would touch and why, the steps in order, the risks, and how you would verify it works. Record the steps with TaskCreate. Do NOT edit files or run commands -- those tools are refused until the human turns plan mode off (/plan off).";

/// Told whenever plan mode turns on (`true`) or off (`false`), however it was
/// switched. Called on the task that ran the command; keep it short.
pub type PlanListener = Arc<dyn Fn(bool) + Send + Sync>;

/// PLAN MODE (built in): read-only planning until the human turns it off.
///
/// `/plan [what to plan]` -- or a host's plan toggle, which runs the same
/// command through [`Mods::command`](crate::Mods::command) -- turns it on;
/// `/plan off` turns it off. A topic after `/plan` goes to the model as
/// `Plan this (plan mode is on): <topic>`; a bare `/plan` sends nothing. Both
/// switches tell the human with a notice.
///
/// Two halves, because instructions alone are only a request:
/// - the system prompt gains the planning brief, [`PLAN_PROMPT`];
/// - every tool that is not read-only is refused, except the planning tools
///   themselves ([`PLANNING_TOOLS`] unless the host names others with
///   [`with_planning_tools`](Self::with_planning_tools)). Subagents run
///   through the same mods, so they are read-only too.
///
/// [`load_mods`](crate::load_mods) always adds it, off.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::{MemFs, ShutWorkspace, ToolContext, workspace::WorkspaceMode};
/// use haest_mods::{CommandOutcome, ModConfig, ModTools, PLAN_PROMPT, load_mods};
/// use serde_json::json;
/// # use haest_mods::{ApprovalRequest, Decision, ModUi};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
/// #     fn notice(&self, text: &str) { println!("{text}") }
/// # }
/// # struct Write;
/// # #[async_trait::async_trait]
/// # impl haest_core::Tool for Write {
/// #     fn name(&self) -> &str { "Write" }
/// #     fn description(&self) -> String { "Write a file".into() }
/// #     fn parameters(&self) -> serde_json::Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "file.write" }
/// #     async fn execute(&self, _: haest_core::tool::JsonMap, _: &ToolContext)
/// #         -> Result<haest_core::ToolOutput, haest_core::ToolError> {
/// #         Ok(haest_core::ToolOutput::text("written"))
/// #     }
/// # }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let config = ModConfig::new("/project", WorkspaceMode::Sandbox, "/home/me/.config/haest");
/// let mods = Arc::new(load_mods(config, &MemFs::new(), Arc::new(Terminal)).await);
/// let tools = ModTools::wrap(&mods, vec![Arc::new(Write)]);
/// let ctx = ToolContext::new("/project", Arc::new(ShutWorkspace));
/// let write = || tools[0].execute(json!({ "file_path": "notes.md" }).as_object().cloned().unwrap(), &ctx);
///
/// // `/plan the migration`: plan mode on, and the topic goes to the model.
/// let sent = mods.command("/plan", "the migration").await;
/// assert!(matches!(sent, CommandOutcome::Prompt(text) if text.contains("the migration")));
/// assert!(mods.system_append().await.contains(PLAN_PROMPT));
/// assert!(write().await.is_err());
///
/// assert_eq!(mods.command("/plan", "off").await, CommandOutcome::Handled);
/// assert!(write().await.is_ok());
/// # }
/// ```
pub struct PlanMod {
    on: AtomicBool,
    on_change: Option<PlanListener>,
    planning_tools: Vec<String>,
}

impl Default for PlanMod {
    /// [`PlanMod::new`] with no listener.
    fn default() -> Self {
        Self::new(None)
    }
}

impl PlanMod {
    /// Plan mode, off, with the [`PLANNING_TOOLS`]. `on_change` tells the
    /// host, so its plan toggle shows haest's state however it was switched.
    pub fn new(on_change: Option<PlanListener>) -> Self {
        Self {
            on: AtomicBool::new(false),
            on_change,
            planning_tools: PLANNING_TOOLS.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// Plan mode with these planning tools in place of [`PLANNING_TOOLS`]:
    /// the tools, by name, that run while it is on though they are not
    /// read-only (a host's own task list, say).
    pub fn with_planning_tools(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.planning_tools = names.into_iter().map(Into::into).collect();
        self
    }

    /// Whether plan mode is on right now.
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Mod for PlanMod {
    fn name(&self) -> &str {
        "plan"
    }

    fn commands(&self) -> Vec<String> {
        vec![PLAN_COMMAND.into()]
    }

    async fn command(
        &self,
        _name: &str,
        args: &str,
        haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        let args = args.trim();
        let on = args != "off";
        self.on.store(on, Ordering::SeqCst);
        if let Some(f) = &self.on_change {
            f(on);
        }
        haest.notice(if on {
            "Plan mode ON: read-only until /plan off."
        } else {
            "Plan mode off: every tool runs again."
        });
        Ok((on && !args.is_empty()).then(|| format!("Plan this (plan mode is on): {args}")))
    }

    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(self.is_on().then(|| PLAN_PROMPT.to_string()))
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        if self.is_on() && !call.read_only && !self.planning_tools.contains(&call.name) {
            return Ok(Before::Deny(format!(
                "plan mode is on: {} changes things. Finish the plan; the human turns plan mode off with /plan off.",
                call.name
            )));
        }
        Ok(Before::Pass)
    }
}
