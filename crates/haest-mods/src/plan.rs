//! PLAN MODE (built-in mod): `/plan [what to plan]` -- or a host's plan toggle,
//! which runs the same command -- puts haest in read-only planning until
//! `/plan off`. Two halves, because instructions alone are only a request:
//!   - the system prompt gains the planning brief (what a plan skill would say);
//!   - every tool that is not read-only is refused, except the planning tools
//!     themselves (the task list, subagents, ToolSearch). Subagents run through
//!     the same mods, so they are read-only too.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use haest_core::tool::ToolContext;

use crate::api::{Before, Mod, ModEnv, ModError, ModToolCall};

pub const PLAN_COMMAND: &str = "plan";

const PLANNING_TOOLS: &[&str] = &[
    "TaskCreate",
    "TaskUpdate",
    "TaskList",
    "TaskGet",
    "Agent",
    "SendMessage",
    "StopAgent",
    "ToolSearch",
];

pub const PLAN_PROMPT: &str = "# Plan mode is ON\nThe human wants a plan before any change. Explore with read-only tools (Read, Grep, Glob, WebFetch, subagents), then answer with the plan: the goal in a sentence, the files you would touch and why, the steps in order, the risks, and how you would verify it works. Record the steps with TaskCreate. Do NOT edit files or run commands -- those tools are refused until the human turns plan mode off (/plan off).";

/// Told whenever plan mode turns on or off, however it was switched.
pub type PlanListener = Arc<dyn Fn(bool) + Send + Sync>;

#[derive(Default)]
pub struct PlanMod {
    on: AtomicBool,
    on_change: Option<PlanListener>,
}

impl PlanMod {
    /// `on_change` tells the host, so its plan toggle shows haest's state however it was switched.
    pub fn new(on_change: Option<PlanListener>) -> Self {
        Self {
            on: AtomicBool::new(false),
            on_change,
        }
    }

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
    ) -> Result<Option<String>, ModError> {
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

    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, ModError> {
        Ok(self.is_on().then(|| PLAN_PROMPT.to_string()))
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, ModError> {
        if self.is_on() && !call.read_only && !PLANNING_TOOLS.contains(&call.name.as_str()) {
            return Ok(Before::Deny(format!(
                "plan mode is on: {} changes things. Finish the plan; the human turns plan mode off with /plan off.",
                call.name
            )));
        }
        Ok(Before::Pass)
    }
}
