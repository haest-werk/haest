//! [`CapSettings`] and [`SubagentCapSettings`], the caps as settings hold them, and
//! the [`Settings`] methods that turn them into what the loop, the ledger and
//! the subagent hub take.

use std::time::Duration;

use haest_core::{
    caps::ConversationCaps,
    caps::{SubagentCaps, TurnCaps},
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
    /// One turn's wall clock (the `max_turn_minutes` setting); zero = no
    /// cap. Bare: 60 minutes.
    pub max_turn_time: Duration,
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

impl Default for CapSettings {
    /// The bare caps, what [`Settings::bare`] starts from: 2 USD, 100 model
    /// calls and 60 minutes a turn, 6 repeats, 20 USD and 20M unpriced tokens
    /// a conversation, compaction past 400K tokens, the token caps and the
    /// window following the model.
    ///
    /// ```
    /// use haest_settings::{CapSettings, Settings};
    ///
    /// let caps = CapSettings { max_turn_usd: 5.0, ..CapSettings::default() };
    /// assert_eq!(caps.max_turn_steps, Settings::bare("/w", "/h").caps.max_turn_steps);
    /// ```
    fn default() -> Self {
        Self {
            max_turn_usd: 2.0,
            max_turn_tokens: None,
            max_request_tokens: None,
            max_turn_steps: 100,
            max_turn_time: Duration::from_secs(60 * 60),
            max_repeats: 6,
            max_conversation_usd: 20.0,
            max_conversation_tokens: 20_000_000,
            context_window: None,
            compact_at: 400_000,
        }
    }
}

/// The subagent tree's caps as settings hold them: the `max_agent_depth`,
/// `max_subagents`, `subagent_usd`, `subagent_steps`, `subagent_minutes` and
/// `subagent_budgets` settings. [`Settings::subagent_caps`] turns them into haest-core's
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
    /// One subagent's wall clock (the `subagent_minutes` setting).
    pub time: Duration,
    /// The model may give a subagent tighter caps (the Agent tool's
    /// `budget`), held on the conversation's budget while it runs. Bare: off.
    pub budgets: bool,
}

impl Default for SubagentCapSettings {
    /// haest-core's [`SubagentCaps::default`].
    fn default() -> Self {
        let c = SubagentCaps::default();
        Self {
            max_depth: c.max_depth,
            max_concurrent: c.max_concurrent,
            usd: c.usd,
            steps: c.steps,
            time: c.time,
            budgets: c.budgets,
        }
    }
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
    /// An unset token cap follows the window: `max_turn_tokens` is 10x it
    /// (at most `u64::MAX`), `max_request_tokens` 90% of it. One set to
    /// `Some(0)` stays 0, no cap. The window itself goes along, for a
    /// threshold on the context's size.
    pub fn turn_caps_for(&self, window: u64) -> TurnCaps {
        let l = &self.caps;
        // 90% is never more than the window, so it fits back in a u64.
        let ninety_percent = (u128::from(window) * 9 / 10) as u64;
        TurnCaps {
            max_turn_usd: l.max_turn_usd,
            max_turn_tokens: l.max_turn_tokens.unwrap_or(window.saturating_mul(10)),
            max_request_tokens: l.max_request_tokens.unwrap_or(ninety_percent),
            max_turn_steps: l.max_turn_steps,
            max_turn_time: l.max_turn_time,
            max_repeats: l.max_repeats,
            context_window: window,
            subagent: false,
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

    /// The caps on the subagent tree, for haest-tools' hub: `subagents` as
    /// they are.
    pub fn subagent_caps(&self) -> SubagentCaps {
        let s = &self.subagents;
        SubagentCaps {
            max_depth: s.max_depth,
            max_concurrent: s.max_concurrent,
            usd: s.usd,
            steps: s.steps,
            time: s.time,
            budgets: s.budgets,
        }
    }
}
