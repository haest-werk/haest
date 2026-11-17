//! haest-core's two sites, [`TurnSite`] and [`LedgerSite`], and their
//! defaults: the guard's nudge and the ledger's warning.

use super::{Action, Mark, Meter, Reading, Rearm, Scope, Site, Threshold, ThresholdRef};

/// The share of a turn cap at which the guard nudges the model: half.
pub const NUDGE_SHARE: f64 = 0.5;

/// The share of a conversation cap at which the ledger warns the human.
pub const WARN_SHARE: f64 = 0.8;

/// What every guard nudge ends with, once.
pub(crate) const WAY_OUT: &str = "If you are making progress, carry on. If you are repeating yourself or waiting on something that will not change, stop: try a different approach, or end your turn and tell the human what is blocking you.";

/// Seconds as the guard says them: whole minutes.
pub(crate) fn minutes(secs: f64) -> String {
    format!("{} min", (secs / 60.0).round())
}

/// Checked during a turn, by the loop, on
/// [`SharedDeps::thresholds`](crate::SharedDeps::thresholds): Remind, Warn and
/// WindDown at every step boundary (after the tool results), Stop before
/// every model call. Reads every meter but [`Meter::Turns`]; cannot compact.
///
/// Its defaults are the guard's nudge: at half of the turn's step, time, USD
/// and token caps ([`NUDGE_SHARE`]), the model is reminded once a turn what
/// it is burning. Each agent keeps its own [`Watch`](super::Watch), as long
/// as the agent lives.
#[derive(Debug, Clone, Copy)]
pub struct TurnSite;

impl Site for TurnSite {
    const NAME: &'static str = "during a turn";

    fn serves(meter: Meter, action: Action) -> bool {
        meter != Meter::Turns && action != Action::Compact
    }

    fn defaults() -> Vec<ThresholdRef> {
        [
            Meter::Steps,
            Meter::WallTime,
            Meter::Usd(Scope::Turn),
            Meter::Tokens(Scope::Turn),
        ]
        .into_iter()
        .map(|meter| ThresholdRef::new(CapNudge(meter)))
        .collect()
    }
}

/// The guard's nudge on one turn cap.
struct CapNudge(Meter);

impl Threshold for CapNudge {
    fn meter(&self) -> Meter {
        self.0
    }
    fn mark(&self) -> Mark {
        Mark::share(NUDGE_SHARE)
    }
    fn action(&self) -> Action {
        Action::Remind
    }
    fn rearm(&self) -> Rearm {
        Rearm::NextTurn
    }
    fn text(&self, r: &Reading) -> String {
        let (value, cap) = (r.value, r.cap.unwrap_or_default());
        match self.0 {
            Meter::Steps => format!(
                "You have made {} model calls this turn; the turn stops at {}.",
                value as u64, cap as u64
            ),
            Meter::WallTime => format!(
                "This turn has run for {}; it stops at {}.",
                minutes(value),
                minutes(cap)
            ),
            Meter::Usd(_) => format!("This turn has spent ${value:.2}; it stops at ${cap}."),
            _ => format!(
                "This turn has billed {} tokens; it stops at {}.",
                value as u64, cap as u64
            ),
        }
    }
    fn closing(&self) -> Option<&str> {
        Some(WAY_OUT)
    }
}

/// Checked at every charge, by the
/// [`Ledger`](crate::ledger::Ledger) ([`with_thresholds`](crate::ledger::Ledger::with_thresholds)):
/// reads the conversation's USD and unpriced tokens, and warns the human
/// ([`LedgerHooks::warn`](crate::ledger::LedgerHooks::warn)). A cap the human
/// sets, and a ledger restored from the session log, settle it
/// ([`Watch::settle`](super::Watch::settle)).
///
/// Its defaults are the ledger's warning: at [`WARN_SHARE`] of each cap, once
/// per crossing.
#[derive(Debug, Clone, Copy)]
pub struct LedgerSite;

impl Site for LedgerSite {
    const NAME: &'static str = "at a ledger charge";

    fn serves(meter: Meter, action: Action) -> bool {
        matches!(
            meter,
            Meter::Usd(Scope::Conversation) | Meter::Tokens(Scope::Conversation)
        ) && action == Action::Warn
    }

    fn defaults() -> Vec<ThresholdRef> {
        [
            Meter::Usd(Scope::Conversation),
            Meter::Tokens(Scope::Conversation),
        ]
        .into_iter()
        .map(|meter| ThresholdRef::new(BudgetWarning(meter)))
        .collect()
    }
}

/// The ledger's warning on one conversation cap.
struct BudgetWarning(Meter);

impl Threshold for BudgetWarning {
    fn meter(&self) -> Meter {
        self.0
    }
    fn mark(&self) -> Mark {
        Mark::share(WARN_SHARE)
    }
    fn action(&self) -> Action {
        Action::Warn
    }
    fn rearm(&self) -> Rearm {
        Rearm::BelowMark
    }
    fn text(&self, r: &Reading) -> String {
        format!(
            "Conversation budget: {}. haest stops at the cap.",
            crate::ledger::spent(r)
        )
    }
}
