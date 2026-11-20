//! THRESHOLDS: a meter crossing a mark fires an action. One mechanism behind
//! the guard's nudge, the ledger's warning, the agent-turn gate's nudge,
//! compaction and wind-down, and a host or a mod adds its own.
//!
//! A [`Threshold`] is four things:
//!
//! - a [`Meter`]: what it reads (context tokens, steps, turns, wall time, USD,
//!   tokens), each in a turn's or the conversation's [`Scope`]; a fixed list;
//! - a [`Mark`]: where it fires, a share of the meter's cap or a value;
//! - an [`Action`]: remind the model, warn the human, wind down (no new
//!   subagents), compact, stop the turn;
//! - a [`Rearm`]: when it may fire again.
//!
//! A threshold is checked where its meter is read, a [`Site`], which holds
//! its own list ([`Thresholds`]) and remembers which have fired ([`Watch`]).
//! A site refuses a threshold it cannot check ([`NotServed`]):
//!
//! | Site | Checks | Reads | Can |
//! |---|---|---|---|
//! | [`TurnSite`]: [`SharedDeps::thresholds`](crate::SharedDeps::thresholds) | at every step boundary (stop: before every model call) | context tokens, steps, wall time, USD and tokens of the turn and the conversation | remind, warn, wind down, stop |
//! | [`LedgerSite`]: [`Ledger::set_thresholds`](crate::ledger::Ledger::set_thresholds) | at every charge | the conversation's USD and unpriced tokens | warn |
//! | haest-tools' `AgentTurnSite`: `AgentTurnGate::with_thresholds` | at every turn no human started | turns | remind, warn |
//! | compaction: [`needs_compaction`](crate::compaction::needs_compaction) | before every model call, from the host's hook | context tokens | compact; [`CompactionOptions`](crate::compaction::CompactionOptions) is the threshold |
//!
//! Every site starts from its defaults, which are what haest did before
//! thresholds, with the same numbers. Nothing changes until a host changes them:
//!
//! | Default | Meter | Mark | Action | Rearm |
//! |---|---|---|---|---|
//! | the guard's nudge | steps, wall time, the turn's USD and tokens | half the cap ([`NUDGE_SHARE`]) | remind | next turn |
//! | the ledger's warning | the conversation's USD and unpriced tokens | [`WARN_SHARE`] of the cap | warn | below mark |
//! | the agent-turn gate's nudge | turns | half the cap | remind | next turn |
//! | compaction | context tokens | [`COMPACT_SHARE`](crate::compaction::COMPACT_SHARE) of the window, or a set value when lower | compact | every check |
//! | wind-down, off unless set ([`WindDown`](crate::wind_down::WindDown)) | the conversation's USD and unpriced tokens, the turn's USD, tokens and wall time | the set share | wind down | next turn |
//!
//! Not thresholds: the caps themselves (a turn stops AT a cap, a Stop
//! threshold stops it earlier), the request cap (per request, not a meter),
//! and the guard's repeat check (one count per distinct tool call).
//!
//! # Examples
//!
//! A host's own threshold: at 60% of the model's window the model is told,
//! once, to write down what it knows. It fires again only after the context
//! falls back under the mark (after a compaction, say).
//!
//! ```
//! use haest_core::threshold::{
//!     Action, Mark, Meter, Reading, Rearm, Threshold, Thresholds, TurnSite,
//! };
//!
//! # fn main() -> Result<(), haest_core::threshold::NotServed> {
//!
//! struct SaveNotes;
//!
//! impl Threshold for SaveNotes {
//!     fn meter(&self) -> Meter { Meter::ContextTokens }
//!     fn mark(&self) -> Mark { Mark::share(0.6) }
//!     fn action(&self) -> Action { Action::Remind }
//!     fn rearm(&self) -> Rearm { Rearm::BelowMark }
//!     fn text(&self, r: &Reading) -> String {
//!         format!("The context holds {} tokens: write what you know to NOTES.md.", r.value)
//!     }
//! }
//!
//! // The guard's nudges, and this one.
//! let thresholds = Thresholds::<TurnSite>::default().try_with(SaveNotes)?;
//! assert_eq!(thresholds.len(), 5);
//!
//! // A turn cannot compact: compaction is the host's, before a model call.
//! struct Squeeze;
//! impl Threshold for Squeeze {
//!     fn meter(&self) -> Meter { Meter::ContextTokens }
//!     fn mark(&self) -> Mark { Mark::at(100_000.0) }
//!     fn action(&self) -> Action { Action::Compact }
//!     fn rearm(&self) -> Rearm { Rearm::EveryCheck }
//!     fn text(&self, _: &Reading) -> String { String::new() }
//! }
//! assert!(Thresholds::<TurnSite>::none().try_with(Squeeze).is_err());
//! # Ok(())
//! # }
//! ```
//!
//! `cargo run -p haest --example threshold` runs one in a whole turn.

mod sites;
#[cfg(test)]
mod tests;
mod watch;

use std::fmt;

pub use sites::{LedgerSite, NUDGE_SHARE, TurnSite, WARN_SHARE};
pub(crate) use sites::{WAY_OUT, minutes};
pub(crate) use watch::Parts;
pub use watch::{NotServed, Site, Thresholds, Watch};

use crate::remedy::{Limit, Remedies};

crate::word_enum! {
    /// Whose meter: one turn's, or the whole conversation's.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Scope ("scope") {
        /// One turn: it starts over when the next turn starts.
        Turn = "turn",
        /// The whole conversation: posting again does not reset it.
        Conversation = "conversation",
    }
}

/// What a threshold reads. A fixed list: each meter is read where haest
/// already counts it, in the unit given here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Meter {
    /// What the last model call sent, in tokens (its
    /// [`Usage::context_tokens`](crate::Usage::context_tokens)): how full the
    /// context is. Its cap is the model's window
    /// ([`TurnCaps::context_window`](crate::caps::TurnCaps::context_window)).
    /// Conversation scope.
    ContextTokens,
    /// Model calls this turn. Cap: [`TurnCaps::max_turn_steps`](crate::caps::TurnCaps::max_turn_steps).
    Steps,
    /// Turns in a row that no human message started (haest-tools'
    /// `AgentTurnGate` counts them). Conversation scope.
    Turns,
    /// How long this turn has run, in seconds, on the host's
    /// [`Clock`](crate::Clock). Cap: [`TurnCaps::max_turn_time`](crate::caps::TurnCaps::max_turn_time).
    WallTime,
    /// USD spent: the turn's (cap [`TurnCaps::max_turn_usd`](crate::caps::TurnCaps::max_turn_usd)),
    /// or the conversation's (cap [`ConversationCaps::max_usd`](crate::caps::ConversationCaps::max_usd)).
    Usd(Scope),
    /// Tokens billed: the turn's, by every call (cap
    /// [`TurnCaps::max_turn_tokens`](crate::caps::TurnCaps::max_turn_tokens)),
    /// or the conversation's, by calls that reported no price (cap
    /// [`ConversationCaps::max_unpriced_tokens`](crate::caps::ConversationCaps::max_unpriced_tokens)).
    Tokens(Scope),
}

impl Meter {
    /// Whose meter it is.
    pub fn scope(self) -> Scope {
        match self {
            Meter::Steps | Meter::WallTime => Scope::Turn,
            Meter::ContextTokens | Meter::Turns => Scope::Conversation,
            Meter::Usd(scope) | Meter::Tokens(scope) => scope,
        }
    }
}

impl fmt::Display for Meter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Meter::ContextTokens => "context tokens",
            Meter::Steps => "steps",
            Meter::Turns => "turns",
            Meter::WallTime => "wall time",
            Meter::Usd(Scope::Turn) => "the turn's USD",
            Meter::Usd(Scope::Conversation) => "the conversation's USD",
            Meter::Tokens(Scope::Turn) => "the turn's tokens",
            Meter::Tokens(Scope::Conversation) => "the conversation's unpriced tokens",
        })
    }
}

/// Where on its meter a threshold fires: a share of the meter's cap, a
/// value in the meter's unit, or whichever of the two comes first.
///
/// ```
/// use haest_core::threshold::Mark;
///
/// assert_eq!(Mark::share(0.8).value(Some(1000.0)), Some(800.0));
/// assert_eq!(Mark::share(0.8).value(None), None); // no cap, nothing to take a share of
/// assert_eq!(Mark::at(300.0).value(None), Some(300.0));
/// assert_eq!(Mark::share(0.8).or_at(300.0).value(Some(1000.0)), Some(300.0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    share: Option<f64>,
    at: Option<f64>,
}

impl Mark {
    /// At `share` of the meter's cap (0.8 = 80%). On a meter with no cap it
    /// never fires.
    pub const fn share(share: f64) -> Self {
        Self {
            share: Some(share),
            at: None,
        }
    }

    /// At `value`, in the meter's unit, whatever the cap.
    pub const fn at(value: f64) -> Self {
        Self {
            share: None,
            at: Some(value),
        }
    }

    /// Also at `value`: the mark is whichever comes first.
    #[must_use]
    pub fn or_at(self, value: f64) -> Self {
        Self {
            at: Some(self.at.map_or(value, |at| at.min(value))),
            ..self
        }
    }

    /// Where it sits on a meter capped at `cap` (None = no cap); None = nowhere.
    pub fn value(&self, cap: Option<f64>) -> Option<f64> {
        let share = self.share.zip(cap).map(|(share, cap)| cap * share);
        match (share, self.at) {
            (Some(share), Some(at)) => Some(share.min(at)),
            (share, at) => share.or(at),
        }
    }

    /// `reading` is at or past it.
    pub fn reached(&self, reading: &Reading) -> bool {
        self.value(reading.cap)
            .is_some_and(|mark| reading.value >= mark)
    }
}

crate::word_enum! {
    /// What a threshold does when its meter reaches its mark.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[non_exhaustive]
    pub enum Action ("threshold action") {
        /// Tell the model, at the next step boundary, as input it reads (the
        /// human sees it too, as a [`LoopEvent::Guard`](crate::LoopEvent::Guard)).
        Remind = "remind",
        /// Tell the human; the model does not see it.
        Warn = "warn",
        /// The turn stops starting new work: haest-tools' Agent tool starts no
        /// new subagent ([`ToolContext::wind_down`](crate::ToolContext::wind_down)),
        /// and the model is told once a turn to conclude.
        WindDown = "wind down",
        /// Compact the conversation (the host's, before a model call).
        Compact = "compact",
        /// End the turn before its next model call.
        Stop = "stop",
    }
}

crate::word_enum! {
    /// When a threshold that fired may fire again.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Rearm ("rearm") {
        /// Once, for as long as its site lives.
        Never = "never",
        /// Once a turn: it re-arms when the next turn starts.
        NextTurn = "next turn",
        /// Once each time the meter climbs past the mark: it re-arms when a
        /// check finds the meter back under it (a cap raised, a compacted
        /// context).
        BelowMark = "below mark",
        /// At every check the meter is at or past the mark: a level, not an
        /// edge. Compaction's: what a check finds nothing to cut is tried again
        /// at the next one.
        EveryCheck = "every check",
    }
}

/// A meter as a site read it, when it checked.
///
/// Built with [`Reading::new`] and its `with_*` setters, so a field added
/// later breaks no site of a host's; its fields read as they are.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Reading {
    /// What was read.
    pub meter: Meter,
    /// Its value, in the meter's unit ([`Meter`] says which).
    pub value: f64,
    /// The meter's cap in force; None = no cap (0 turns a cap off).
    pub cap: Option<f64>,
    /// That cap as a host's [`Remedies`] knows it; None when it has none (the
    /// model's window).
    pub limit: Option<Limit>,
}

impl Reading {
    /// `meter` at `value`, in the meter's unit, with no cap and no limit.
    pub fn new(meter: Meter, value: f64) -> Self {
        Self {
            meter,
            value,
            cap: None,
            limit: None,
        }
    }

    /// The meter's cap in force (0 is a cap that is off, as no cap).
    #[must_use]
    pub fn with_cap(mut self, cap: f64) -> Self {
        self.cap = (cap > 0.0).then_some(cap);
        self
    }

    /// That cap as a host's [`Remedies`] knows it.
    #[must_use]
    pub fn with_limit(mut self, limit: Limit) -> Self {
        self.limit = Some(limit);
        self
    }

    /// `text`, then the host's remedy for this meter's cap ([`Self::limit`]).
    pub fn remedied(&self, text: String, remedies: &Remedies) -> String {
        match self.limit {
            Some(limit) => remedies.add_to(text, limit),
            None => text,
        }
    }
}

/// PLUG POINT: one threshold. haest's defaults implement it, and so does a
/// host's or a mod's own; a site checks it ([`Thresholds`]).
///
/// Its [`meter`](Self::meter), [`mark`](Self::mark), [`action`](Self::action)
/// and [`rearm`](Self::rearm) are what it is: a [`Thresholds`] list reads
/// them once, when it takes it, and the site checks those. A threshold whose
/// mark should move goes to its site again, in a new list. Its
/// [`text`](Self::text) and [`closing`](Self::closing) run when it fires,
/// under no lock of haest's.
///
/// A threshold's `Drop` must not panic: it may run while the agent holds its
/// threshold list, and a panic there leaves every later turn of that agent
/// panicking too. Nothing is lost: a threshold holds no conversation data, and
/// history, the session file, the ledger and the journal stay as they were.
pub trait Threshold: Send + Sync {
    /// What it reads.
    fn meter(&self) -> Meter;
    /// Where it fires.
    fn mark(&self) -> Mark;
    /// What it does.
    fn action(&self) -> Action;
    /// When it may fire again.
    fn rearm(&self) -> Rearm;
    /// What it says when it fires: what the model reads (Remind; for
    /// WindDown, the fact before "no new subagents"), what the human reads
    /// (Warn), why the turn stopped (Stop). The site ends a warning, a stop
    /// and a refusal with the host's remedy for [`Reading::limit`]. Compact
    /// says nothing: the host says what it compacted.
    fn text(&self, reading: &Reading) -> String;
    /// A sentence said once after every reminder due at the same moment that
    /// ends with it, however many do (the guard's nudges share their way out
    /// of a loop). None by default.
    fn closing(&self) -> Option<&str> {
        None
    }
}

impl fmt::Debug for dyn Threshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Threshold")
            .field("meter", &self.meter())
            .field("mark", &self.mark())
            .field("action", &self.action())
            .field("rearm", &self.rearm())
            .finish()
    }
}

crate::plug_ref! {
    /// A [`Threshold`], shared: what a [`Thresholds`] list holds. Built from
    /// the threshold itself, or from an `Arc` of one.
    pub struct ThresholdRef(dyn Threshold);
    debug(threshold, f) { fmt::Debug::fmt(threshold, f) }
}

/// A threshold that fired, and the reading it fired on.
#[derive(Clone)]
pub struct Crossing {
    threshold: ThresholdRef,
    /// The threshold as its site checked it: what Debug shows.
    parts: Parts,
    reading: Reading,
}

/// The threshold as its site checked it, and the reading.
impl fmt::Debug for Crossing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Crossing")
            .field("threshold", &self.parts)
            .field("reading", &self.reading)
            .finish()
    }
}

impl Crossing {
    /// The threshold.
    pub fn threshold(&self) -> &ThresholdRef {
        &self.threshold
    }

    /// What its meter read.
    pub fn reading(&self) -> &Reading {
        &self.reading
    }

    /// What the threshold says about this reading ([`Threshold::text`]).
    pub fn text(&self) -> String {
        self.threshold.text(&self.reading)
    }

    /// Its text, then the host's remedy for the meter's cap: what a warning,
    /// a stop and a refusal say.
    pub fn remedied_text(&self, remedies: &Remedies) -> String {
        self.reading.remedied(self.text(), remedies)
    }
}
