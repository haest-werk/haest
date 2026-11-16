//! THE APPROVAL MOD (built in): in the REAL workspace, a Bash command that can
//! destroy work the human cannot get back waits for their Allow. The sandbox
//! never asks -- its writes live in memory and vanish anyway. A host turns it
//! off with `ModConfig::approvals`.
//!
//! The list is about irreversibility, not about danger in general: deleting
//! recursively, rewriting git history or the remote, wiping a disk, killing
//! processes by name, sudo. Anything else runs as before -- unless auto mode is
//! on ([`AutoJudge`]), which judges every command and may ask, or let a
//! git-recoverable delete through.

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use kern_core::{tool::ToolContext, workspace::WorkspaceMode};
use regex::bytes::{Regex, RegexBuilder};
use serde_json::Value;

use crate::api::{ApprovalRequest, Before, Decision, Mod, ModEnv, ModError, ModToolCall};

/// ASCII classes, the way the JavaScript patterns these came from match.
fn re(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .unicode(false)
        .build()
        .expect("a valid built-in pattern")
}

static IRREVERSIBLE: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        // A flag anywhere in the same command, as GNU rm takes it: `rm -v -rf`,
        // `rm -R`, `rm --recursive`, `rm build -rf`.
        (
            re(
                r"\brm\s(?:[^;&|\n]*\s)?(?:-[a-zA-Z]*[rRf][a-zA-Z]*|--recursive|--force)(?:\s|$|[;&|])",
            ),
            "deletes files recursively or by force",
        ),
        (
            re(r"\bfind\b.*\s-delete\b"),
            "deletes every file it finds (find -delete)",
        ),
        (
            re(r"\bgit\s+push\b.*\s(--force\b|--force-with-lease\b|-f\b|\+\S)"),
            "force-pushes, rewriting the remote",
        ),
        (
            re(r"\bgit\s+reset\s+--hard\b"),
            "discards uncommitted work (git reset --hard)",
        ),
        (
            re(r"\bgit\s+clean\s+-[a-zA-Z]*f"),
            "deletes untracked files (git clean)",
        ),
        (
            re(r"\bgit\s+(branch\s+-D|stash\s+(drop|clear))\b"),
            "deletes a branch or stash",
        ),
        (
            re(r"\bgit\s+checkout\s+--\s"),
            "discards file changes (git checkout --)",
        ),
        // The whole tree (`.`, `./`, `:/`), flags anywhere. Also asks for
        // `git restore --staged .`, which only unstages: the safe side.
        (
            re(r"\bgit\s+(?:checkout|restore)\s(?:[^;&|\n]*\s)?(?:\./?|:/)(?:\s|$|[;&|])"),
            "discards every uncommitted change (git checkout . / git restore .)",
        ),
        (
            re(r"\b(mkfs|shred)\b|\bdd\s+if="),
            "writes over a disk or file irrecoverably",
        ),
        (re(r"\b(pkill|killall)\b|\bkill\s+-9\b"), "kills processes"),
        (re(r"(^|[;&|]\s*)sudo\b"), "runs as root"),
    ]
});

/// Why `command` needs the human's go-ahead, or None.
pub fn irreversible_reason(command: &str) -> Option<&'static str> {
    IRREVERSIBLE
        .iter()
        .find(|(re, _)| re.is_match(command.as_bytes()))
        .map(|(_, why)| *why)
}

/// What auto mode makes of one Bash command.
#[derive(Debug, Clone, PartialEq)]
pub enum BashVerdict {
    /// Run it; `note` (if any) tells the human what auto mode let through.
    Run { note: Option<String> },
    /// Ask the human first; the reason completes "a command that ...".
    Ask(String),
}

/// PLUG POINT: auto mode's judge. When it is on it judges every Bash command
/// instead of the irreversible list alone.
#[async_trait]
pub trait AutoJudge: Send + Sync {
    fn on(&self) -> bool;
    async fn judge(&self, command: &str, ctx: &ToolContext) -> BashVerdict;
}

#[derive(Default)]
pub struct ApprovalMod {
    auto: Option<Arc<dyn AutoJudge>>,
}

impl ApprovalMod {
    /// `auto`, when on, judges every command instead of the list alone.
    pub fn new(auto: Option<Arc<dyn AutoJudge>>) -> Self {
        Self { auto }
    }

    /// Why this command must ask, or None to let it run.
    async fn ask_why(&self, command: &str, ctx: &ToolContext, kern: &ModEnv) -> Option<String> {
        let Some(auto) = self.auto.as_ref().filter(|a| a.on()) else {
            return irreversible_reason(command).map(str::to_string);
        };
        match auto.judge(command, ctx).await {
            BashVerdict::Ask(why) => Some(why),
            BashVerdict::Run { note } => {
                if let Some(note) = note {
                    kern.notice(&note);
                }
                None
            }
        }
    }
}

/// The command as the shell would get it: a string, or whatever was sent, as text.
fn command_of(call: &ModToolCall) -> String {
    match call.input.get("command") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

#[async_trait]
impl Mod for ApprovalMod {
    fn name(&self) -> &str {
        "approval"
    }

    /// The command judged is the one that runs, after every mod's rewrite.
    fn is_gate(&self) -> bool {
        true
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        ctx: &ToolContext,
        kern: &ModEnv,
    ) -> Result<Before, ModError> {
        if kern.workspace != WorkspaceMode::Real || call.name != "Bash" {
            return Ok(Before::Pass);
        }
        let command = command_of(call);
        let Some(why) = self.ask_why(&command, ctx, kern).await else {
            return Ok(Before::Pass);
        };
        let req = ApprovalRequest {
            tool_name: "Bash".into(),
            description: format!("kern wants to run a command that {why}"),
            input_preview: command.chars().take(200).collect(),
            tool_use_id: None,
            key: Some(command),
        };
        Ok(match kern.ask(req, &ctx.cancel).await {
            Decision::Allow => Before::Pass,
            Decision::Deny => Before::Deny(format!(
                "the human did not approve this command (it {why}). Ask them, or find another way."
            )),
        })
    }
}

#[cfg(test)]
mod tests;
