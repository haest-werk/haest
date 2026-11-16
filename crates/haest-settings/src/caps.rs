//! [`CapSettings`] and [`SubagentCapSettings`], the caps as settings hold them, and
//! the [`Settings`] methods that turn them into what the loop, the ledger and
//! the subagent hub take.

use std::time::Duration;

use haest_core::{
    caps::{SubagentCaps, TurnCaps},
    ledger::ConversationCaps,
};

use crate::Settings;

/// The caps on one turn (one human message) and on the whole conversation.
///
/// 0 means no cap, for every field. A token cap left `None` follows the
/// model's context window instead; [`Settings::turn_caps_for`] turns these into
/// the loop's per-turn [`TurnCaps`] and [`Settings::conversation_caps`] into the
/// conversation's [`ConversationCaps`]. The loop checks them before each model call;
/// the human goes past a turn cap by sending another message.
#[derive(Debug, Clone, PartialEq)]
pub struct CapSettings {
    /// One turn's spend in USD; only calls whose cost is known count. Bare: 2.
    pub max_turn_usd: f64,
    /// One turn's billed tokens; None = 10x the model's window.
    pub max_turn_tokens: Option<u64>,
    /// One model call's estimated input tokens; None = 90% of the window.
    pub max_request_tokens: Option<u64>,
    /// Model calls in one turn. Bare: 100.
    pub max_turn_steps: u32,
    /// One turn's wall clock, in minutes. Bare: 60.
    pub max_turn_minutes: f64,
    /// The same tool call in a row before the guard stops it. Bare: 6.
    pub max_repeats: u32,
    /// The whole conversation's spend in USD. Bare: 20.
    pub max_conversation_usd: f64,
    /// Tokens billed by models with no price, across the conversation. Bare: 20M.
    pub max_conversation_tokens: u64,
    /// The model's context window in tokens; None (or 1000 or less) = the
    /// catalog's, else 200K. See [`Settings::window`].
    pub context_window: Option<u64>,
    /// Compact the conversation past this many tokens; 0 = at 80% of the window only. Bare: 400K.
    pub compact_at: u64,
}

/// The subagent tree's caps as settings hold them: the `max_agent_depth`,
/// `max_subagents`, `subagent_usd`, `subagent_steps` and `subagent_minutes`
/// settings. [`Settings::subagent_caps`] turns them into haest-core's
/// [`SubagentCaps`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubagentCapSettings {
    /// How deep agents may nest (main = 0); 0 = no Agent tool.
    pub max_depth: u32,
    /// Subagents running at once, across the tree; 0 = no Agent tool.
    pub max_concurrent: usize,
    /// One subagent's spend in USD.
    pub usd: f64,
    /// One subagent's model calls.
    pub steps: u32,
    /// One subagent's wall clock, in minutes.
    pub minutes: u64,
}

impl Default for SubagentCapSettings {
    /// haest-core's [`SubagentCaps::default`], in minutes.
    fn default() -> Self {
        let c = SubagentCaps::default();
        Self {
            max_depth: c.max_depth,
            max_concurrent: c.max_concurrent,
            usd: c.usd,
            steps: c.steps,
            minutes: c.time.as_secs() / 60,
        }
    }
}

/// A duration of `m` minutes; a negative or non-finite one is zero (no cap).
fn minutes(m: f64) -> Duration {
    Duration::try_from_secs_f64(m * 60.0).unwrap_or(Duration::ZERO)
}

impl Settings {
    /// The model's context window in tokens: [`CapSettings::context_window`] when
    /// set above 1000, else `catalog_window` (the catalog row's, for a model
    /// the catalog knows), else 200K.
    ///
    /// ```
    /// use haest_settings::Settings;
    ///
    /// let mut s = Settings::bare("/w", "/h");
    /// assert_eq!(s.window(None), 200_000);
    /// assert_eq!(s.window(Some(1_000_000)), 1_000_000);
    /// s.caps.context_window = Some(50_000);
    /// assert_eq!(s.window(Some(1_000_000)), 50_000);
    /// ```
    pub fn window(&self, catalog_window: Option<u64>) -> u64 {
        let set = self.caps.context_window.filter(|n| *n > 1000);
        set.or(catalog_window).unwrap_or(200_000)
    }

    /// The loop's per-turn caps for a model with this `window` (from
    /// [`Settings::window`]).
    ///
    /// An unset token cap follows the window: `max_turn_tokens` is 10x it,
    /// `max_request_tokens` 90% of it. One set to `Some(0)` stays 0, no cap.
    /// A negative or non-finite `max_turn_minutes` is no cap.
    pub fn turn_caps_for(&self, window: u64) -> TurnCaps {
        let l = &self.caps;
        TurnCaps {
            max_turn_usd: l.max_turn_usd,
            max_turn_tokens: l.max_turn_tokens.unwrap_or(window * 10),
            max_request_tokens: l.max_request_tokens.unwrap_or(window * 9 / 10),
            max_turn_steps: l.max_turn_steps,
            max_turn_time: minutes(l.max_turn_minutes),
            max_repeats: l.max_repeats,
        }
    }

    /// The caps that span the whole conversation, for haest-core's ledger:
    /// `max_conversation_usd` and `max_conversation_tokens`.
    pub fn conversation_caps(&self) -> ConversationCaps {
        ConversationCaps {
            max_usd: self.caps.max_conversation_usd,
            max_unpriced_tokens: self.caps.max_conversation_tokens,
        }
    }

    /// The caps on the subagent tree, for haest-tools' hub: `subagents` with
    /// its minutes as a duration.
    pub fn subagent_caps(&self) -> SubagentCaps {
        let s = &self.subagents;
        SubagentCaps {
            max_depth: s.max_depth,
            max_concurrent: s.max_concurrent,
            usd: s.usd,
            steps: s.steps,
            time: Duration::from_secs(s.minutes.saturating_mul(60)),
        }
    }
}
