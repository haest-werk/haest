//! How the human gets past a limit, in the host's own words.
//!
//! haest's messages about a limit say what happened and what the cap is, and
//! never name a host's command, setting or variable: haest-core has no `/budget`
//! and reads no `HAEST_*` variable. A host that has a way past a limit hands
//! haest [`Remedies`], and each message about that [`Limit`] gets the host's
//! remedy after it. One value serves every place that writes such a message:
//! [`SharedDeps::remedies`](crate::SharedDeps::remedies) (the turn caps and the
//! request cap), [`Ledger::set_remedies`](crate::ledger::Ledger::set_remedies)
//! (the conversation budget), haest-tools' `AgentTurnGate::with_remedies` and
//! haest-workspace's `SandboxWorkspace::with_remedies`.
//!
//! # Examples
//!
//! ```
//! use haest_core::{
//!     caps::ConversationCap,
//!     remedy::{Limit, Remedies},
//! };
//!
//! let remedies = Remedies::new(|limit| match limit {
//!     Limit::Conversation(ConversationCap::Usd(cap)) => {
//!         Some(format!("Type /budget {} to double it.", cap * 2.0))
//!     }
//!     _ => None,
//! });
//!
//! let text = remedies.add_to("The budget is spent.", Limit::Conversation(ConversationCap::Usd(5.0)));
//! assert_eq!(text, "The budget is spent. Type /budget 10 to double it.");
//! // No remedy for this one: haest's text alone.
//! assert_eq!(remedies.add_to("Stopped.", Limit::TurnSteps(40)), "Stopped.");
//! ```

use std::{fmt, sync::Arc, time::Duration};

use crate::caps::ConversationCap;

/// A limit haest holds a conversation to, with its number: a cap a turn, a
/// subagent or the conversation reached (or nears), or the sandbox keeping
/// edits off the disk. What a host's [`Remedies`] answers.
///
/// "Cap" is haest's word for a maximum that stops something; this is named
/// `Limit` because it also covers [`SandboxEdits`](Self::SandboxEdits), which
/// is no cap.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Limit {
    /// A turn made this many model calls ([`TurnCaps::max_turn_steps`](crate::caps::TurnCaps::max_turn_steps)).
    TurnSteps(u32),
    /// A turn ran this long ([`TurnCaps::max_turn_time`](crate::caps::TurnCaps::max_turn_time)).
    TurnTime(Duration),
    /// A turn spent this much ([`TurnCaps::max_turn_usd`](crate::caps::TurnCaps::max_turn_usd)).
    TurnUsd(f64),
    /// A turn billed this many tokens ([`TurnCaps::max_turn_tokens`](crate::caps::TurnCaps::max_turn_tokens)).
    TurnTokens(u64),
    /// One tool call repeated this often ([`TurnCaps::max_repeats`](crate::caps::TurnCaps::max_repeats)).
    Repeats(u32),
    /// A request bigger than this was not sent ([`TurnCaps::max_request_tokens`](crate::caps::TurnCaps::max_request_tokens)).
    RequestTokens(u64),
    /// The conversation reached (or nears) one of its caps, in force at this value.
    Conversation(ConversationCap),
    /// This many turns in a row ran with no message from the human: the rest
    /// are held (haest-tools' `AgentTurnGate`).
    AgentTurns(u32),
    /// The sandbox keeps every edit in memory until the human applies it
    /// (haest-workspace's `SandboxWorkspace`).
    SandboxEdits,
    /// A subagent made this many model calls
    /// ([`SubagentCaps::steps`](crate::caps::SubagentCaps::steps)).
    SubagentSteps(u32),
    /// A subagent ran this long ([`SubagentCaps::time`](crate::caps::SubagentCaps::time)).
    SubagentTime(Duration),
    /// A subagent spent this much ([`SubagentCaps::usd`](crate::caps::SubagentCaps::usd)).
    SubagentUsd(f64),
}

impl Limit {
    /// The plain name of the setting that sets this limit (`max_turn_steps`),
    /// as haest's own messages name it. haest-config's registry has a row of
    /// this name for every limit.
    ///
    /// ```
    /// use haest_core::remedy::Limit;
    ///
    /// assert_eq!(Limit::TurnSteps(40).setting(), "max_turn_steps");
    /// assert_eq!(Limit::SubagentUsd(0.5).setting(), "subagent_usd");
    /// ```
    pub fn setting(&self) -> &'static str {
        match self {
            Limit::TurnSteps(_) => "max_turn_steps",
            Limit::TurnTime(_) => "max_turn_minutes",
            Limit::TurnUsd(_) => "max_turn_usd",
            Limit::TurnTokens(_) => "max_turn_tokens",
            Limit::Repeats(_) => "max_repeats",
            Limit::RequestTokens(_) => "max_request_tokens",
            Limit::Conversation(ConversationCap::Usd(_)) => "max_conversation_usd",
            Limit::Conversation(ConversationCap::UnpricedTokens(_)) => "max_conversation_tokens",
            Limit::AgentTurns(_) => "max_agent_turns",
            Limit::SandboxEdits => "workspace",
            Limit::SubagentSteps(_) => "subagent_steps",
            Limit::SubagentTime(_) => "subagent_minutes",
            Limit::SubagentUsd(_) => "subagent_usd",
        }
    }
}

type Remedy = dyn Fn(Limit) -> Option<String> + Send + Sync;

/// PLUG POINT: a host's way past each [`Limit`] (its command, its setting),
/// added after haest's own message about it. `Default` has none.
#[derive(Clone, Default)]
pub struct Remedies(Option<Arc<Remedy>>);

impl Remedies {
    /// Remedies that `remedy` writes: the text for a limit (a sentence or
    /// more), or None when the host has none for it.
    pub fn new(remedy: impl Fn(Limit) -> Option<String> + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(remedy)))
    }

    /// The host's remedy for `limit`, if it has one.
    pub fn get(&self, limit: Limit) -> Option<String> {
        self.0.as_ref().and_then(|r| r(limit))
    }

    /// `text`, then the host's remedy for `limit` a space after it; `text`
    /// alone when there is none.
    pub fn add_to(&self, text: impl Into<String>, limit: Limit) -> String {
        let text = text.into();
        match self.get(limit) {
            Some(remedy) => format!("{text} {remedy}"),
            None => text,
        }
    }
}

impl fmt::Debug for Remedies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "Remedies(host)"
        } else {
            "Remedies(none)"
        })
    }
}
