//! The human's Allow / Deny with a memory: the generic half of a host's
//! [`crate::ModUi::ask`]. The host supplies the prompt itself ([`Prompter`]: a
//! terminal question, a control panel dialog); the gate keeps the rules every
//! host must keep:
//!   - ALWAYS is scoped to the ask it was pressed on -- the same tool with the
//!     same key (a Bash ask's key is its exact command) -- never the whole tool:
//!     one ALWAYS on `rm -rf node_modules` must not wave through `rm -rf ~`.
//!     Those asks then answer themselves, reported through
//!     [`Prompter::auto_approved`] so the receipt trail stays complete;
//!   - an unanswered ask is a DENY after the timeout, and on interrupt -- a tool
//!     call never hangs forever on a human who walked away;
//!   - an ask under an already-interrupted call is a deny at once, never a prompt.

use std::{collections::HashSet, sync::Mutex, time::Duration};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::api::{ApprovalRequest, Decision};

/// How the human answered one prompt: what a [`Prompter`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Allow this one call.
    Allow,
    /// Allow, and every later ask with the same tool and key.
    AllowAlways,
    /// Refuse this one call.
    Deny,
}

/// PLUG POINT: puts one ask in front of the human -- a terminal question, a
/// dialog, a push notification -- for an [`ApprovalGate`].
///
/// The gate keeps the rules (ALWAYS, the timeout, interrupts); a prompter
/// only asks and reports what the human pressed.
#[async_trait]
pub trait Prompter: Send + Sync {
    /// Ask and wait for the answer. The future is dropped when the ask is
    /// withdrawn (interrupt, timeout): a late answer must go nowhere, so take
    /// the prompt down when it is dropped.
    async fn prompt(&self, req: &ApprovalRequest) -> Answer;
    /// An ask an earlier ALWAYS answered, without a prompt: show it, so the
    /// human's record of what ran stays complete. Default: nothing.
    fn auto_approved(&self, _req: &ApprovalRequest) {}
}

/// How long an [`ApprovalGate`] waits for an answer by default (10 minutes)
/// before the ask is a Deny.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The human's Allow / Deny with a memory: the rules every host's
/// [`ModUi::ask`](crate::ModUi::ask) must keep, over the host's own
/// [`Prompter`].
///
/// - ALWAYS is scoped to the ask it was pressed on: the same
///   [`tool_name`](ApprovalRequest::tool_name) and the same
///   [`key`](ApprovalRequest::key) (the description when there is none; a
///   Bash ask's key is its exact command). One ALWAYS on `rm -rf node_modules`
///   never waves through `rm -rf ~`.
/// - An ask nobody answers is a Deny after the timeout ([`ASK_TIMEOUT`] unless
///   set with [`ApprovalGate::with_timeout`]).
/// - An interrupt is a Deny, and an ask under a call already interrupted is a
///   Deny at once, never a prompt.
///
/// The ALWAYS answers live as long as the gate: one gate per conversation
/// keeps them for that conversation.
///
/// # Examples
///
/// A host built on the gate, with [`ApprovalMod`](crate::ApprovalMod) asking
/// before a Bash command that can destroy work.
///
/// ```
/// use std::sync::{Arc, Mutex};
///
/// use async_trait::async_trait;
/// use haest_core::{ShutWorkspace, ToolContext, workspace::WorkspaceMode};
/// use haest_mods::{
///     Answer, ApprovalGate, ApprovalMod, ApprovalRequest, Decision, ModEnv, ModUi, ModTools,
///     Mods, Prompter,
/// };
/// use serde_json::json;
/// use tokio_util::sync::CancellationToken;
/// # struct Bash;
/// # #[async_trait]
/// # impl haest_core::Tool for Bash {
/// #     fn name(&self) -> &str { "Bash" }
/// #     fn description(&self) -> String { "Run a command".into() }
/// #     fn parameters(&self) -> serde_json::Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "shell.exec" }
/// #     async fn execute(&self, i: haest_core::tool::JsonMap, _: &ToolContext)
/// #         -> Result<haest_core::ToolOutput, haest_core::ToolError> {
/// #         Ok(haest_core::ToolOutput::text(format!("ran {}", i["command"])))
/// #     }
/// # }
///
/// /// The human: presses ALWAYS on the first prompt and Deny on every other.
/// struct Human(Arc<Mutex<Vec<String>>>);
///
/// #[async_trait]
/// impl Prompter for Human {
///     async fn prompt(&self, req: &ApprovalRequest) -> Answer {
///         let mut seen = self.0.lock().unwrap();
///         seen.push(req.input_preview.clone());
///         if seen.len() == 1 { Answer::AllowAlways } else { Answer::Deny }
///     }
/// }
///
/// /// How the mods reach the human: asks go through the gate, notices to the
/// /// terminal.
/// struct Terminal(ApprovalGate<Human>);
///
/// #[async_trait]
/// impl ModUi for Terminal {
///     async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
///         self.0.ask(req, cancel).await
///     }
///     fn notice(&self, text: &str) {
///         println!("{text}")
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let prompts = Arc::new(Mutex::new(Vec::new()));
/// let ui = Arc::new(Terminal(ApprovalGate::new(Human(prompts.clone()))));
/// let mut mods = Mods::new(ModEnv::new("/project", WorkspaceMode::Real, ui));
/// mods.add(Arc::new(ApprovalMod::default()));
/// let tools = ModTools::wrap(&Arc::new(mods), vec![Arc::new(Bash)]);
///
/// let ctx = ToolContext::new("/project", Arc::new(ShutWorkspace));
/// let bash = |command: &str| tools[0].execute(json!({ "command": command }).as_object().cloned().unwrap(), &ctx);
/// assert!(bash("ls").await.is_ok()); // nothing to ask
/// assert!(bash("rm -rf build").await.is_ok()); // asked: ALWAYS
/// assert!(bash("rm -rf build").await.is_ok()); // the same command: no prompt
/// assert!(bash("rm -rf ~").await.is_err()); // another command: asked again, denied
/// assert_eq!(*prompts.lock().unwrap(), ["rm -rf build", "rm -rf ~"]);
/// # }
/// ```
pub struct ApprovalGate<P> {
    prompter: P,
    always: Mutex<HashSet<String>>,
    timeout: Duration,
}

fn rule_key(req: &ApprovalRequest) -> String {
    let key = req.key.as_deref().unwrap_or(&req.description);
    format!("{}\n{key}", req.tool_name)
}

impl<P: Prompter> ApprovalGate<P> {
    /// A gate over `prompter`, with no ALWAYS answers yet and the default
    /// [`ASK_TIMEOUT`].
    pub fn new(prompter: P) -> Self {
        Self {
            prompter,
            always: Mutex::default(),
            timeout: ASK_TIMEOUT,
        }
    }

    /// The gate with another timeout: how long an ask waits for the human
    /// before it is a Deny.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The prompter the gate asks through.
    pub fn prompter(&self) -> &P {
        &self.prompter
    }

    /// The human's decision on `req`.
    ///
    /// An ask an earlier ALWAYS covers is an Allow at once (reported through
    /// [`Prompter::auto_approved`]). Otherwise the prompter asks; the ask is
    /// withdrawn -- its future dropped -- and a Deny returned when `cancel`
    /// fires or the timeout passes. A `cancel` that fired before the call is a
    /// Deny with no prompt at all.
    pub async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        // Checked first: a prompt raised now would only ever be withdrawn.
        if cancel.is_cancelled() {
            return Decision::Deny;
        }
        let key = rule_key(&req);
        if self.always.lock().unwrap().contains(&key) {
            self.prompter.auto_approved(&req);
            return Decision::Allow;
        }
        let answer = tokio::select! {
            a = self.prompter.prompt(&req) => a,
            _ = cancel.cancelled() => Answer::Deny,
            _ = tokio::time::sleep(self.timeout) => Answer::Deny,
        };
        match answer {
            Answer::Allow => Decision::Allow,
            Answer::AllowAlways => {
                self.always.lock().unwrap().insert(key);
                Decision::Allow
            }
            Answer::Deny => Decision::Deny,
        }
    }

    /// Forgets every ALWAYS given for `tool_name` (the human took them back):
    /// every ask of that tool prompts again.
    pub fn revoke(&self, tool_name: &str) {
        let prefix = format!("{tool_name}\n");
        self.always
            .lock()
            .unwrap()
            .retain(|k| !k.starts_with(&prefix));
    }
}

#[cfg(test)]
mod tests;
