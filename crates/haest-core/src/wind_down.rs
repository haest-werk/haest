//! WIND-DOWN (opt-in): when spend reaches a set share of a cap, a run stops
//! starting new work and finishes with what it has. It is one thing a
//! [`threshold`](crate::threshold) can do ([`Action::WindDown`]); the
//! `wind_down_at` setting ([`WindDown`]) turns on a set of them. Two things
//! happen:
//!
//! 1. a tool that would start new work refuses: it asks its
//!    [`ToolContext::wind_down`](crate::ToolContext::wind_down) before it
//!    starts (haest-tools' Agent tool starts no new subagent);
//! 2. the model gets ONE notice a turn, at the next step boundary: conclude
//!    with what it has, start no new work. It reaches the model the way the
//!    turn guard's nudge does, and the human sees it as a
//!    [`LoopEvent::Guard`](crate::LoopEvent::Guard).
//!
//! The caps [`WindDown`] watches: the conversation's [`Ledger`](crate::ledger::Ledger)
//! (USD, unpriced tokens) and the turn's USD, tokens and wall time
//! ([`TurnCaps`](crate::caps::TurnCaps)). A subagent's one turn has its own
//! caps (its `SubagentCaps`) and shares the ledger, so each subagent winds
//! down on its own. A cap that is off (0) is never watched. A host's own
//! wind-down threshold (at 40 steps, say) works the same way.
//!
//! The guard's nudge says "you are burning X, find the exit" at half of each
//! turn cap; wind-down changes what the run may do. A tool reads it through
//! [`WindDownWatch`]; what it says is a [`WindingDown`].

mod fact;
mod watch;

use std::sync::Arc;

use crate::threshold::{
    Action, Mark, Meter, Reading, Rearm, Scope, Threshold, Thresholds, TurnSite,
};

pub use fact::WindingDown;
pub use watch::WindDownWatch;

/// The share of a cap at which a run winds down: between 0 and 1, where 0 is
/// off (the default). The `wind_down_at` setting; [`Thresholds::wind_down`]
/// adds its thresholds to a turn's.
///
/// # Examples
///
/// A conversation with a $1 budget that winds down at 80%: its first model
/// call spends $0.85 and asks for a tool, the tool sees the run winding down,
/// and the model is told once to conclude.
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, ShutWorkspace, Tool, ToolContext, ToolError,
///     ToolOutput, Tools, Usage,
///     caps::ConversationCaps,
///     ledger::{Ledger, LedgerHooks, LedgerSnapshot},
///     testing::{ScriptedProvider, text_answer, tool_answer},
///     threshold::Thresholds,
///     tool::JsonMap,
///     wind_down::WindDown,
/// };
/// use serde_json::{Value, json};
///
/// struct Quiet;
/// impl LedgerHooks for Quiet {
///     fn persist(&self, _: &LedgerSnapshot) {}
///     fn warn(&self, _: &str) {}
/// }
///
/// /// Starts new work, unless the run is winding down.
/// struct Fan;
///
/// #[async_trait::async_trait]
/// impl Tool for Fan {
///     fn name(&self) -> &str { "Fan" }
///     fn description(&self) -> String { "Start more work.".into() }
///     fn parameters(&self) -> Value { json!({"type": "object"}) }
///     fn kind(&self) -> &str { "task.spawn" }
///     async fn execute(&self, _: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
///         match ctx.wind_down().check() {
///             Some(w) => Err(ToolError::Input(w.refusal("no new work"))),
///             None => Ok(ToolOutput::text("started")),
///         }
///     }
/// }
///
/// let mut asks = tool_answer(&[("c1", "Fan", json!({}))]);
/// asks.usage = Usage { cost_usd: Some(0.85), ..Usage::default() };
/// let provider = ScriptedProvider::new([asks, text_answer("Here is what I have.")]);
/// let budget = ConversationCaps { max_usd: 1.0, max_unpriced_tokens: 0 };
/// let ledger = Ledger::new(budget, Quiet);
/// let deps = AgentDeps::new(provider.resolver(), Tools::new().with(Fan), "", "/", ShutWorkspace)
///     .with_ledger(ledger.clone())
///     .with_thresholds(Thresholds::default().wind_down(WindDown::at(0.8)?));
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
///
/// let mut events = Vec::new();
/// agent.run("fan out", &mut events).await;
///
/// let refused = events.iter().any(|e| matches!(e, LoopEvent::ToolResult { output, .. } if output.is_error));
/// let notices = events.iter().filter(|e| matches!(e, LoopEvent::Guard { .. })).count();
/// assert!(refused);
/// assert_eq!(notices, 1);
/// assert_eq!(ledger.usd(), 0.85);
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WindDown {
    at: f64,
}

/// A wind-down share that is not a number between 0 and 1.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("a wind-down share is a number from 0 to 1 (0 = off), not {share}")]
pub struct BadShare {
    /// The share, as given.
    pub share: f64,
}

/// The caps it watches, in the order a refusal names the first one past.
const WATCHED: [Meter; 5] = [
    Meter::Usd(Scope::Conversation),
    Meter::Tokens(Scope::Conversation),
    Meter::Usd(Scope::Turn),
    Meter::Tokens(Scope::Turn),
    Meter::WallTime,
];

impl WindDown {
    /// Never winds down: the default.
    pub const OFF: Self = Self { at: 0.0 };

    /// Wind down at `share` of every cap; 0 is off.
    ///
    /// # Errors
    ///
    /// [`BadShare`] for anything but a number from 0 to 1.
    ///
    /// ```
    /// use haest_core::wind_down::WindDown;
    ///
    /// assert!(WindDown::at(0.8).unwrap().is_on());
    /// assert_eq!(WindDown::at(0.0).unwrap(), WindDown::OFF);
    /// assert!(WindDown::at(8.0).is_err());
    /// ```
    pub fn at(share: f64) -> Result<Self, BadShare> {
        match (0.0..=1.0).contains(&share) {
            true => Ok(Self { at: share }),
            false => Err(BadShare { share }),
        }
    }

    /// The share, 0 when off.
    pub fn share(self) -> f64 {
        self.at
    }

    /// It winds down at all.
    pub fn is_on(self) -> bool {
        self.at > 0.0
    }

    /// Its thresholds: one per watched cap, none when off.
    pub fn thresholds(self) -> Vec<Arc<dyn Threshold>> {
        if !self.is_on() {
            return Vec::new();
        }
        let at = |meter| {
            Arc::new(WindDownAt {
                meter,
                share: self.share_for(meter),
            })
        };
        WATCHED
            .into_iter()
            .map(|m| at(m) as Arc<dyn Threshold>)
            .collect()
    }

    /// The share at which `meter`'s cap winds down. The one place that says
    /// which share applies to which cap: today one share serves them all.
    fn share_for(self, _meter: Meter) -> f64 {
        self.at
    }
}

impl Thresholds<TurnSite> {
    /// These, and `at`'s wind-down after them (none when it is off).
    ///
    /// ```
    /// use haest_core::{threshold::{Thresholds, TurnSite}, wind_down::WindDown};
    ///
    /// let off = Thresholds::<TurnSite>::default().wind_down(WindDown::OFF);
    /// let on = Thresholds::<TurnSite>::default().wind_down(WindDown::at(0.8).unwrap());
    /// assert_eq!(on.len() - off.len(), 5);
    /// ```
    pub fn wind_down(self, at: WindDown) -> Self {
        // A turn reads every watched meter and can wind down.
        self.served(at.thresholds())
    }
}

/// Wind-down on one cap, at a share of it.
struct WindDownAt {
    meter: Meter,
    share: f64,
}

impl Threshold for WindDownAt {
    fn meter(&self) -> Meter {
        self.meter
    }
    fn mark(&self) -> Mark {
        Mark::share(self.share)
    }
    fn action(&self) -> Action {
        Action::WindDown
    }
    fn rearm(&self) -> Rearm {
        Rearm::NextTurn
    }
    fn text(&self, reading: &Reading) -> String {
        fact::nearly_spent(reading)
    }
}

#[cfg(test)]
mod tests;
