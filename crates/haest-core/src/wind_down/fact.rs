//! [`WindingDown`]: what a refusal and the notice say.

use std::fmt;

use crate::{
    remedy::{Limit, Remedies},
    threshold::{Crossing, Meter, Reading},
};

/// A run past a wind-down threshold's mark: the fact a refusal and the
/// notice state.
#[derive(Debug, Clone, PartialEq)]
pub struct WindingDown {
    /// What was spent of which cap: "the budget is nearly spent: $8.10 of
    /// $10 (`max_conversation_usd`)". The threshold's text.
    pub fact: String,
    /// The cap, at its value in force: what a host's [`Remedies`] answers;
    /// None when the meter has none.
    pub limit: Option<Limit>,
    /// The host's way past the cap ([`Remedies::get`]), if it has one.
    pub remedy: Option<String>,
}

impl WindingDown {
    /// The wind-down `crossing` states, with the host's remedy for its cap.
    pub(crate) fn of(crossing: &Crossing, remedies: &Remedies) -> Self {
        let limit = crossing.reading().limit;
        Self {
            fact: crossing.text(),
            limit,
            remedy: limit.and_then(|l| remedies.get(l)),
        }
    }

    /// What a tool that refuses new work tells the model: the fact, `then`
    /// (what it refuses), and the host's remedy after it.
    ///
    /// ```
    /// use haest_core::{remedy::Limit, wind_down::WindingDown};
    ///
    /// let w = WindingDown {
    ///     fact: "the budget is nearly spent: $8.10 of $10 (`max_turn_usd`)".into(),
    ///     limit: Some(Limit::TurnUsd(10.0)),
    ///     remedy: Some("Ask the human.".into()),
    /// };
    /// let text = w.refusal("no new subagents");
    /// assert!(text.contains("max_turn_usd") && text.ends_with(" Ask the human."));
    /// ```
    pub fn refusal(&self, then: &str) -> String {
        let text = format!("{self}; {then}.");
        match &self.remedy {
            Some(remedy) => format!("{text} {remedy}"),
            None => text,
        }
    }

    /// The notice the model gets once a turn.
    pub(crate) fn notice(&self) -> String {
        format!(
            "[haest wind-down] {self}. Conclude with what you have: start no new work, and end your turn with your answer."
        )
    }
}

/// The fact.
impl fmt::Display for WindingDown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.fact)
    }
}

/// What `wind_down_at`'s thresholds say: "the budget is nearly spent: $8.10
/// of $10 (`max_conversation_usd`)", "the time is nearly up: 24 min of 30 min
/// (`max_turn_minutes`)".
pub(super) fn nearly_spent(r: &Reading) -> String {
    let setting = r
        .limit
        .map(|l| format!(" (`{}`)", l.setting()))
        .unwrap_or_default();
    let (spent, cap) = (r.value, r.cap.unwrap_or_default());
    match r.meter {
        Meter::WallTime => format!(
            "the time is nearly up: {} of {}{setting}",
            wall_clock(spent),
            wall_clock(cap)
        ),
        Meter::Usd(_) => format!("the budget is nearly spent: ${spent:.2} of ${cap}{setting}"),
        _ => format!("the budget is nearly spent: {spent} of {cap} tokens{setting}"),
    }
}

/// Seconds as a reader says them: whole minutes from two minutes up, else seconds.
fn wall_clock(secs: f64) -> String {
    match secs >= 120.0 {
        true => format!("{} min", (secs / 60.0).round()),
        false => format!("{} s", secs.round()),
    }
}
