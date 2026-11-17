//! THE APPROVAL MOD (built in): in the REAL workspace, a Bash command that can
//! destroy work the human cannot get back waits for their Allow. The sandbox
//! never asks -- its writes live in memory and vanish anyway. A host turns it
//! off with [`ModOptions::with_approvals`](crate::ModOptions::with_approvals)`(false)`.
//!
//! The list is about irreversibility, not about danger in general: deleting
//! recursively, rewriting git history or the remote, wiping a disk, killing
//! processes by name, sudo. Anything else runs as before -- unless auto mode is
//! on ([`AutoJudge`]), which judges every command and may ask, or let a
//! git-recoverable delete through.

use std::sync::LazyLock;

use async_trait::async_trait;
use haest_core::{error::BoxError, tool::ToolContext, workspace::WorkspaceMode};
use regex::bytes::{Regex, RegexBuilder};
use serde_json::Value;

use crate::api::{ApprovalRequest, Before, Decision, Mod, ModEnv, ModToolCall};

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

/// Why a shell `command` can destroy work the human cannot get back, or None
/// when it cannot.
///
/// The reason completes the sentence "a command that ...": "deletes files
/// recursively or by force", "force-pushes, rewriting the remote". The list
/// is about irreversibility, not danger in general: deleting recursively or
/// by force (`rm -rf`, `find -delete`), rewriting git history or the remote
/// (`push --force`, `reset --hard`, `clean -f`, `branch -D`, `stash drop`,
/// `checkout -- <file>`, `checkout .`, `restore .`), writing over a disk
/// (`mkfs`, `shred`, `dd if=`), killing processes by name or with `-9`, and
/// `sudo`. A match anywhere in the line counts, so `cd x && rm -rf y` asks.
/// It is a pattern match on the text, not a shell parser: it errs toward
/// asking (`git restore --staged .` asks, though it only unstages).
///
/// [`ApprovalMod`] asks the human about exactly these commands unless auto
/// mode judges instead.
///
/// # Examples
///
/// ```
/// use haest_mods::irreversible_reason;
///
/// assert!(irreversible_reason("rm -v -rf build").is_some());
/// assert!(irreversible_reason("git push origin +main").is_some());
/// assert!(irreversible_reason("make && sudo make install").is_some());
/// assert_eq!(irreversible_reason("rm notes.txt"), None);
/// assert_eq!(irreversible_reason("git push"), None);
/// ```
pub fn irreversible_reason(command: &str) -> Option<&'static str> {
    IRREVERSIBLE
        .iter()
        .find(|(re, _)| re.is_match(command.as_bytes()))
        .map(|(_, why)| *why)
}

/// What auto mode's [`AutoJudge`] makes of one Bash command.
#[derive(Debug, Clone, PartialEq)]
pub enum BashVerdict {
    /// Run it without asking.
    Run {
        /// A line the human sees about what auto mode let through, if any.
        note: Option<String>,
    },
    /// Ask the human first; the reason completes "a command that ...".
    Ask(String),
}

/// PLUG POINT: auto mode's judge for [`ApprovalMod`]. When it is on, it
/// judges every Bash command in the real workspace instead of the
/// irreversible list ([`irreversible_reason`]) alone: it may ask about more,
/// or let a delete through that git can bring back.
///
/// haest ships no judge (the TypeScript haest's is not ported); a host that
/// has one hands it in through [`ApprovalMod::with_auto`] or
/// [`ModOptions::with_auto`](crate::ModOptions::with_auto), and haest holds
/// it in an [`AutoJudgeRef`].
#[async_trait]
pub trait AutoJudge: Send + Sync {
    /// Whether auto mode is on right now. Asked before every command, so a
    /// host can switch it mid-conversation; off = the irreversible list decides.
    fn on(&self) -> bool;
    /// The verdict on `command`, about to run in `ctx`. `ctx.cancel()` fires
    /// when the human interrupts the call.
    async fn judge(&self, command: &str, ctx: &ToolContext) -> BashVerdict;
}

haest_core::plug_ref! {
    /// An [`AutoJudge`], shared.
    pub struct AutoJudgeRef(dyn AutoJudge);
}

/// The shell tools [`ApprovalMod`] judges, unless a host names others
/// ([`ApprovalMod::with_shell_tools`],
/// [`ModOptions::with_shell_tools`](crate::ModOptions::with_shell_tools)): haest's `Bash`.
pub const SHELL_TOOLS: &[&str] = &["Bash"];

/// THE APPROVAL MOD (built in): in the REAL workspace, a Bash command that can
/// destroy work the human cannot get back waits for their Allow.
///
/// The sandbox never asks -- its writes live in memory and vanish anyway --
/// and only the shell tools are judged: [`SHELL_TOOLS`] unless the host
/// names others with [`with_shell_tools`](Self::with_shell_tools), each
/// taking its command as a `command` input. Which commands ask is
/// [`irreversible_reason`]'s list, unless auto mode ([`AutoJudge`]) is on.
/// The ask goes to the host's [`ModUi::ask`](crate::ModUi::ask) with the
/// exact command as its key, so an ALWAYS covers that command only; a Deny
/// (or no answer) refuses the call and the model reads why.
///
/// It is a gate ([`Mod::is_gate`]): it judges the command as it will run,
/// after every other mod has rewritten it. [`load_mods`](crate::load_mods)
/// adds it unless
/// [`ModOptions::with_approvals`](crate::ModOptions::with_approvals)`(false)`.
/// [`ApprovalGate`](crate::ApprovalGate#examples)'s example shows it with a
/// whole host.
#[derive(Debug, Clone)]
pub struct ApprovalMod {
    auto: Option<AutoJudgeRef>,
    shell_tools: Vec<String>,
}

impl Default for ApprovalMod {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalMod {
    /// The approval mod over the [`SHELL_TOOLS`], with no auto mode: the
    /// irreversible list decides.
    pub fn new() -> Self {
        Self {
            auto: None,
            shell_tools: SHELL_TOOLS.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// The approval mod with auto mode's judge: while it is
    /// [`on`](AutoJudge::on), it judges every command instead of the list alone.
    #[must_use]
    pub fn with_auto(mut self, judge: impl Into<AutoJudgeRef>) -> Self {
        self.auto = Some(judge.into());
        self
    }

    /// The approval mod judging these tools, by name, in place of
    /// [`SHELL_TOOLS`]: a host whose shell tool has another name. Each takes
    /// the command it runs as its `command` input.
    #[must_use]
    pub fn with_shell_tools(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.shell_tools = names.into_iter().map(Into::into).collect();
        self
    }

    /// Why this command must ask, or None to let it run.
    async fn ask_why(&self, command: &str, ctx: &ToolContext, haest: &ModEnv) -> Option<String> {
        let Some(auto) = self.auto.as_ref().filter(|a| a.on()) else {
            return irreversible_reason(command).map(str::to_string);
        };
        match auto.judge(command, ctx).await {
            BashVerdict::Ask(why) => Some(why),
            BashVerdict::Run { note } => {
                if let Some(note) = note {
                    haest.info(&note);
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
        haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        if haest.workspace() != WorkspaceMode::Real || !self.shell_tools.contains(&call.name) {
            return Ok(Before::Pass);
        }
        let command = command_of(call);
        let Some(why) = self.ask_why(&command, ctx, haest).await else {
            return Ok(Before::Pass);
        };
        let req = ApprovalRequest {
            tool_name: call.name.clone(),
            description: format!("haest wants to run a command that {why}"),
            input_preview: command.chars().take(200).collect(),
            tool_use_id: None,
            key: Some(command),
        };
        Ok(match haest.ask(req, ctx.cancel()).await {
            Decision::Allow => Before::Pass,
            Decision::Deny => Before::Deny(format!(
                "the human did not approve this command (it {why}). Ask them, or find another way."
            )),
        })
    }
}

#[cfg(test)]
mod tests;
