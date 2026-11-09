//! The human's Allow / Deny with a memory: the generic half of a host's
//! [`crate::ModHost::ask`]. The host supplies the prompt itself ([`Prompter`]: a
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

/// How the human answered one prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Allow,
    /// Allow, and every later ask with the same tool and key.
    AllowAlways,
    Deny,
}

/// PLUG POINT: puts one ask in front of the human.
#[async_trait]
pub trait Prompter: Send + Sync {
    /// Ask and wait for the answer. The future is dropped when the ask is
    /// withdrawn (interrupt, timeout): a late answer must go nowhere.
    async fn prompt(&self, req: &ApprovalRequest) -> Answer;
    /// An ask an earlier ALWAYS answered, without a prompt.
    fn auto_approved(&self, _req: &ApprovalRequest) {}
}

/// An unanswered ask is a deny after this long.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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
    pub fn new(prompter: P) -> Self {
        Self {
            prompter,
            always: Mutex::default(),
            timeout: ASK_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn prompter(&self) -> &P {
        &self.prompter
    }

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

    /// The human took ALWAYS back for a tool: every ask of it prompts again.
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
