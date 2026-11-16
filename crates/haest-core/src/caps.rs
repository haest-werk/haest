//! Hard caps on one turn ([`TurnCaps`]), on subagents ([`SubagentCaps`]) and
//! on a whole conversation ([`ConversationCaps`], which the
//! [`Ledger`](crate::ledger::Ledger) enforces).
//! A turn cap is checked by the loop BEFORE every model call -- the only
//! point where stopping keeps history valid (every tool call already has its
//! result). One tool-result flood once cost ~$50 in a single turn; nothing in the
//! harness assumed a turn could get that expensive, so now nothing may.
//!
//! Every cap here is per TURN (one human message): the human continues past a cap
//! by posting again, which is a decision, not an accident. 0 disables a cap. The
//! cap that SPANS turns is the conversation ledger ([`ledger`](crate::ledger));
//! the loop checks all of them before every model call, and nudges the model
//! at half a cap.
//!
//! A host serves [`TurnCaps`] per model spec through a [`TurnCapsSource`]
//! ([`SharedDeps::turn_caps`](crate::SharedDeps::turn_caps)).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{media::messages_for_estimate, messages::Message, provider::ToolSpec, remedy::Limit};

/// The hard caps on one turn. 0 (or a zero duration) turns a cap off;
/// `Default` is every cap off.
///
/// A turn that reaches a cap stops before its next model call with
/// [`StopReason::MaxTokens`](crate::StopReason::MaxTokens) and says which cap
/// in its error; at half a cap the model is nudged first.
/// `max_request_tokens` works per call instead: a call over it is never sent,
/// and the turn ends with [`StopReason::Error`](crate::StopReason::Error).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnCaps {
    /// Stop the turn once it has spent this much (only providers that report cost).
    pub max_turn_usd: f64,
    /// Stop the turn once it has billed this many tokens: the cap that works for EVERY provider.
    pub max_turn_tokens: u64,
    /// Refuse a single model call whose estimated input is bigger than this.
    pub max_request_tokens: u64,
    /// Stop the turn after this many model calls.
    pub max_turn_steps: u32,
    /// Stop the turn once it has run this long (wall clock, on the host's
    /// [`SharedDeps::clock`](crate::SharedDeps::clock)).
    pub max_turn_time: Duration,
    /// Stop the turn once one tool call (same input, same result) has repeated this often.
    pub max_repeats: u32,
    /// The model's context window, in tokens: what a threshold on
    /// [`Meter::ContextTokens`](crate::threshold::Meter::ContextTokens) takes
    /// its share of. 0 = unknown. No turn stops at it: the provider holds the
    /// model to it.
    pub context_window: u64,
    /// These are a subagent's caps: its one turn's USD, model calls and wall
    /// time come from its [`SubagentCaps`], so a message about one of those
    /// names the subagent's setting, and a host's remedy gets
    /// [`Limit::SubagentUsd`], [`Limit::SubagentSteps`] or
    /// [`Limit::SubagentTime`]. haest-tools' hub sets it; false otherwise.
    pub subagent: bool,
}

impl TurnCaps {
    /// The [`Limit`] for `max_turn_usd`: the turn's, or the subagent's.
    pub(crate) fn usd_limit(&self) -> Limit {
        match self.subagent {
            true => Limit::SubagentUsd(self.max_turn_usd),
            false => Limit::TurnUsd(self.max_turn_usd),
        }
    }

    /// The [`Limit`] for `max_turn_steps`: the turn's, or the subagent's.
    pub(crate) fn steps_limit(&self) -> Limit {
        match self.subagent {
            true => Limit::SubagentSteps(self.max_turn_steps),
            false => Limit::TurnSteps(self.max_turn_steps),
        }
    }

    /// The [`Limit`] for `max_turn_time`: the turn's, or the subagent's.
    pub(crate) fn time_limit(&self) -> Limit {
        match self.subagent {
            true => Limit::SubagentTime(self.max_turn_time),
            false => Limit::TurnTime(self.max_turn_time),
        }
    }
}

/// PLUG POINT: the [`TurnCaps`] for a model spec, asked before every model
/// call ([`SharedDeps::turn_caps`](crate::SharedDeps::turn_caps)). The host's
/// policy, apart from what it knows about a model
/// ([`ModelFacts`](crate::ModelFacts)): token caps may follow a model's
/// window. A [`TurnCaps`] is one (the same caps for every model), and so is a
/// closure `Fn(&str) -> Option<TurnCaps>`.
pub trait TurnCapsSource: Send + Sync {
    /// The caps for `spec`; None = uncapped.
    fn turn_caps(&self, spec: &str) -> Option<TurnCaps>;
}

impl TurnCapsSource for TurnCaps {
    fn turn_caps(&self, _spec: &str) -> Option<TurnCaps> {
        Some(self.clone())
    }
}

impl<F: Fn(&str) -> Option<TurnCaps> + Send + Sync> TurnCapsSource for F {
    fn turn_caps(&self, spec: &str) -> Option<TurnCaps> {
        self(spec)
    }
}

/// The caps on the subagent tree. `max_depth` and `max_concurrent` are NOT
/// "0 = off": 0 means no subagents at all (no Agent tool). The three per-agent
/// caps replace the conversation's turn caps for each subagent's one turn.
///
/// The loop never reads them; `haest-tools`' Agent tool enforces them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubagentCaps {
    /// How deep agents may nest (main = 0).
    pub max_depth: u32,
    /// Subagents running at once, across the tree.
    pub max_concurrent: usize,
    /// One subagent's spend.
    pub usd: f64,
    /// One subagent's model calls.
    pub steps: u32,
    /// One subagent's wall clock.
    pub time: Duration,
    /// The model may give a subagent tighter caps (the Agent tool's `budget`):
    /// each one only tightens `usd` and `time`, the spend is held on the
    /// conversation's ledger while it runs, the time must fit what is left of
    /// the caller's turn. The `subagent_budgets` setting; off by default.
    pub budgets: bool,
}

impl Default for SubagentCaps {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_concurrent: 4,
            usd: 1.0,
            steps: 50,
            time: Duration::from_secs(15 * 60),
            budgets: false,
        }
    }
}

/// Rough input size of one request: ~4 characters per token over the text sent,
/// plus each media part's own cost -- base64 counted as text would read a 1 MB
/// screenshot as ~350K tokens and refuse the call.
pub(crate) fn estimate_request_tokens(
    system: &str,
    messages: &[Message],
    tools: &[ToolSpec],
) -> u64 {
    let (json, media_tokens) = messages_for_estimate(messages);
    let tools = serde_json::to_string(tools).unwrap_or_default();
    let chars = system.chars().count() + json.chars().count() + tools.chars().count();
    (chars as u64).div_ceil(4) + media_tokens
}

/// Why this request must not be sent, or None: `estimate` is over
/// [`TurnCaps::max_request_tokens`] and that cap is on.
pub(crate) fn request_too_big(estimate: u64, caps: &TurnCaps) -> Option<String> {
    if caps.max_request_tokens == 0 || estimate <= caps.max_request_tokens {
        return None;
    }
    Some(format!(
        "Model call refused: the request is ~{estimate} tokens, the cap is {}. Compact or clear the conversation, then continue.",
        caps.max_request_tokens
    ))
}

/// A conversation's two caps: the defaults a host configures, or the caps in
/// force ([`Ledger::caps`](crate::ledger::Ledger::caps)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCaps {
    /// USD across the conversation; 0 = no cap.
    pub max_usd: f64,
    /// Tokens billed by calls that reported no price; 0 = no cap.
    pub max_unpriced_tokens: u64,
}

/// The conversation caps the human set; a None field = the configured default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCapOverrides {
    /// The USD cap the human set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    /// The unpriced-token cap the human set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_unpriced_tokens: Option<u64>,
}

impl ConversationCapOverrides {
    pub(crate) fn is_empty(&self) -> bool {
        self.max_usd.is_none() && self.max_unpriced_tokens.is_none()
    }
}

/// One conversation cap the human sets ([`Ledger::set_cap`](crate::ledger::Ledger::set_cap)).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConversationCap {
    /// USD across the conversation; 0 = no cap.
    Usd(f64),
    /// Tokens billed by calls that reported no price; 0 = no cap.
    UnpricedTokens(u64),
}

#[cfg(test)]
mod tests;
