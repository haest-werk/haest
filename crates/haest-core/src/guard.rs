//! THE TURN GUARD: everything that can tell a turn it is going nowhere, read in
//! one place. Each cap has two levels:
//!
//! - NUDGE at half the cap -- one message to the model, once per cap per turn,
//!   injected after the tool results: what it is burning and the way out.
//! - STOP at the cap -- the turn ends before the next model call.
//!
//! The nudge lets a stuck model find the exit on its own; the stop is what makes
//! a stuck model cost a KNOWN amount. Caps: model calls, wall time (the
//! host's clock, [`Clock`](crate::Clock)), turn spend (USD and tokens), REPEATS --
//! the same tool, same input, same result, a model learning nothing (an A,B,A,B
//! cycle counts too) -- and the conversation ledger, which stops but never
//! nudges (it is the human's number, see [`ledger`](crate::ledger)).
//!
//! The guard is the turn's threshold site ([`TurnSite`](crate::threshold::TurnSite),
//! `guard/turn_site.rs`): the nudges on the step, time, USD and token caps
//! are its default thresholds, and a host's own (remind, warn, wind down,
//! stop) run beside them. The repeat check nudges the same way but is no
//! threshold: it keeps one count per distinct call.
//!
//! The caps are passed to every check, read fresh by the caller: the request
//! cap follows the model's window, and the model may change mid-turn.
//!
//! The loop makes a fresh [`TurnGuard`] for every turn; a host sets the caps
//! through a [`TurnCapsSource`](crate::caps::TurnCapsSource) and never drives the guard itself.

mod signature;
mod state;
#[cfg(test)]
mod tests;
mod turn_site;

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use signature::signature;

use crate::{
    caps::{TurnCaps, request_too_big},
    deps::ClockRef,
    ledger::Ledger,
    messages::{ToolCall, Usage},
    remedy::{Limit, Remedies},
    threshold::{NUDGE_SHARE, WAY_OUT, minutes},
    tool::ToolOutput,
};

const CONTINUE: &str = "Post a message to continue; the turn caps reset each turn.";

/// One cap, as the stop reads it.
struct Gauge {
    used: f64,
    cap: f64,
    /// For the human, at the stop.
    stop: String,
    /// Which cap, for the host's remedy at the stop.
    limit: Limit,
}

struct Repeat {
    key: u64,
    name: String,
    count: u64,
}

/// One turn's guard: counts its model calls, its time and its repeated tool
/// calls, charges the conversation ledger, and says when to nudge and when to
/// stop.
pub(crate) struct TurnGuard {
    clock: ClockRef,
    ledger: Option<Arc<Mutex<Ledger>>>,
    remedies: Remedies,
    started: u64,
    steps: u64,
    /// In first-seen order: the order the nudges list them in.
    repeats: Vec<Repeat>,
    /// The repeats nudged this turn.
    nudged: HashSet<u64>,
    /// Floor of the NEXT request: the last one sent, plus what came back since.
    next_request_tokens: u64,
    /// The turn's thresholds: the guard's own nudges unless
    /// [`with_thresholds`](Self::with_thresholds) hands it the agent's.
    turn: turn_site::TurnThresholds,
}

impl TurnGuard {
    /// A fresh guard for one turn; its time cap counts from now on `clock`.
    /// A stop's text ends with the host's remedy for its cap.
    pub(crate) fn new(
        clock: impl Into<ClockRef>,
        ledger: Option<Arc<Mutex<Ledger>>>,
        remedies: Remedies,
    ) -> Self {
        let clock = clock.into();
        let started = clock.now_ms();
        Self {
            clock,
            ledger,
            remedies,
            started,
            steps: 0,
            repeats: Vec::new(),
            nudged: HashSet::new(),
            next_request_tokens: 0,
            turn: Default::default(),
        }
    }

    fn charge(&self, u: &Usage) {
        if let Some(l) = &self.ledger {
            l.lock().unwrap().charge(u);
        }
    }

    /// A model call came back; its bill counts toward the conversation too.
    pub(crate) fn called(&mut self, u: &Usage) {
        self.steps = self.steps.saturating_add(1);
        self.charge(u);
        self.next_request_tokens = self.next_request_tokens.saturating_add(u.output);
        self.turn.context = u.context_tokens();
    }

    /// A tool answered: its own spend is charged, and an identical call+result is counted.
    pub(crate) fn answered(&mut self, call: &ToolCall, out: &ToolOutput) {
        if let Some(c) = out.cost_usd.filter(|c| *c != 0.0) {
            self.charge(&Usage {
                cost_usd: Some(c),
                ..Default::default()
            });
        }
        self.turn.answered(out);
        let key = signature(call, out);
        match self.repeats.iter_mut().find(|r| r.key == key) {
            Some(r) => r.count = r.count.saturating_add(1),
            None => self.repeats.push(Repeat {
                key,
                name: call.name.clone(),
                count: 1,
            }),
        }
        let result_tokens = (out.content.chars().count() as u64).div_ceil(4);
        self.next_request_tokens = self.next_request_tokens.saturating_add(result_tokens);
    }

    /// Why the turn must stop before the next model call, or None: the
    /// ledger, a cap, then a threshold that stops it.
    pub(crate) fn stop(&self, usage: &Usage, caps: Option<&TurnCaps>) -> Option<String> {
        let ledger = self.ledger.as_ref();
        if let Some(why) = ledger.and_then(|l| l.lock().unwrap().exhausted()) {
            return Some(format!("Turn stopped: {why}"));
        }
        let at_cap = self
            .gauges(usage, caps)
            .into_iter()
            .find(|g| g.cap > 0.0 && g.used >= g.cap)
            .map(|g| {
                let text = format!("Turn stopped: {} {CONTINUE}", g.stop);
                self.remedies.add_to(text, g.limit)
            });
        at_cap.or_else(|| self.threshold_stop(usage, caps))
    }

    /// The nudge due now, or None: every repeat at half its cap and every
    /// reminder due (each once per turn), in one message.
    pub(crate) fn nudge(&mut self, usage: &Usage, caps: Option<&TurnCaps>) -> Option<String> {
        let repeats = self.repeat_nudges(caps);
        let (reminders, mut closings) = self.reminders(usage, caps);
        if repeats.is_empty() && reminders.is_empty() {
            return None;
        }
        if !repeats.is_empty() && !closings.iter().any(|c| c == WAY_OUT) {
            closings.insert(0, WAY_OUT.to_string());
        }
        let text = [repeats, reminders, closings].concat();
        Some(format!("[haest guard] {}", text.join(" ")))
    }

    /// Records the request about to go out (its estimated input tokens); why
    /// it must not, or None.
    pub(crate) fn too_big(&mut self, estimate: u64, caps: Option<&TurnCaps>) -> Option<String> {
        self.next_request_tokens = estimate;
        let caps = caps?;
        let text = request_too_big(estimate, caps)?;
        Some(
            self.remedies
                .add_to(text, Limit::RequestTokens(caps.max_request_tokens)),
        )
    }

    /// What is left of the turn's wall-clock cap now; None when uncapped.
    pub(crate) fn time_left(&self, caps: Option<&TurnCaps>) -> Option<Duration> {
        let cap = caps?.max_turn_time;
        let elapsed = self.clock.now_ms().saturating_sub(self.started);
        (!cap.is_zero()).then(|| cap.saturating_sub(Duration::from_millis(elapsed)))
    }

    /// Tokens one more tool result may add before the next request is refused by
    /// `too_big`; None when uncapped.
    pub(crate) fn room(&self, caps: Option<&TurnCaps>) -> Option<u64> {
        let cap = caps?.max_request_tokens;
        (cap > 0).then(|| cap.saturating_sub(self.next_request_tokens))
    }

    fn gauges(&self, usage: &Usage, caps: Option<&TurnCaps>) -> Vec<Gauge> {
        let Some(l) = caps else { return Vec::new() };
        let elapsed = self.clock.now_ms().saturating_sub(self.started) as f64;
        let usd = usage.cost_usd.unwrap_or(0.0);
        let tokens = usage.context_tokens();
        let steps = self.steps;
        let time_cap = l.max_turn_time.as_millis() as f64;
        let mut out = self.repeat_gauges(l.max_repeats);
        out.extend([
            Gauge {
                used: steps as f64,
                cap: l.max_turn_steps as f64,
                stop: format!(
                    "it made {steps} model calls, the cap is {}.",
                    l.max_turn_steps
                ),
                limit: l.steps_limit(),
            },
            Gauge {
                used: elapsed,
                cap: time_cap,
                stop: format!(
                    "it ran for {}, the cap is {}.",
                    minutes(elapsed / 1000.0),
                    minutes(time_cap / 1000.0)
                ),
                limit: l.time_limit(),
            },
            Gauge {
                used: usd,
                cap: l.max_turn_usd,
                stop: format!("it spent ${usd:.2}, the cap is ${}.", l.max_turn_usd),
                limit: l.usd_limit(),
            },
            Gauge {
                used: tokens as f64,
                cap: l.max_turn_tokens as f64,
                stop: format!(
                    "it billed {tokens} tokens, the cap is {}.",
                    l.max_turn_tokens
                ),
                limit: Limit::TurnTokens(l.max_turn_tokens),
            },
        ]);
        out
    }

    fn repeat_gauges(&self, cap: u32) -> Vec<Gauge> {
        self.repeats
            .iter()
            .map(|Repeat { name, count, .. }| Gauge {
                used: *count as f64,
                cap: cap as f64,
                stop: format!("{name} was called {count} times with identical input and returned the identical result each time -- haest was stuck in a loop (the cap is {cap} repeats)."),
                limit: Limit::Repeats(cap),
            })
            .collect()
    }

    /// Each repeat at half its cap (at least two calls), once per turn.
    fn repeat_nudges(&mut self, caps: Option<&TurnCaps>) -> Vec<String> {
        let Some(cap) = caps.map(|c| c.max_repeats).filter(|cap| *cap > 0) else {
            return Vec::new();
        };
        let nudge_at = (cap as f64 * NUDGE_SHARE).ceil().max(2.0);
        let due = self
            .repeats
            .iter()
            .filter(|r| r.count as f64 >= nudge_at && !self.nudged.contains(&r.key));
        let due: Vec<(u64, String)> = due
            .map(|Repeat { key, name, count }| (*key, format!("You have called {name} with identical input {count} times this turn and got the identical result every time -- you look stuck in a loop, and calling it again will not change the answer. At {cap} repeats the turn is stopped.")))
            .collect();
        self.nudged.extend(due.iter().map(|(key, _)| *key));
        due.into_iter().map(|(_, text)| text).collect()
    }
}
