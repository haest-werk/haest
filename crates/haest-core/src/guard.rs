//! THE TURN GUARD: everything that can tell a turn it is going nowhere, read in
//! one place. Each signal has two levels:
//!
//! - NUDGE at half the cap -- one message to the model, once per signal per turn,
//!   injected after the tool results: what it is burning and the way out.
//! - STOP at the cap -- the turn ends before the next model call.
//!
//! The nudge lets a stuck model find the exit on its own; the stop is what makes
//! a stuck model cost a KNOWN amount. Signals: model calls, wall time (the
//! host's clock, [`Clock`](crate::Clock)), turn spend (USD and tokens), REPEATS --
//! the same tool, same input, same result, a model learning nothing (an A,B,A,B
//! cycle counts too) -- and the conversation ledger, which stops but never
//! nudges (it is the human's number, see [`ledger`](crate::ledger)).
//!
//! The caps are passed to every check, read fresh by the caller: the request
//! cap follows the model's window, and the model may change mid-turn.
//!
//! The loop makes a fresh [`TurnGuard`] for every turn; a host sets the caps
//! through [`ModelFacts::turn_caps`](crate::ModelFacts::turn_caps) and never drives the guard itself.

use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use serde_json::{Map, Value};

use crate::{
    caps::{TurnCaps, request_too_big},
    deps::Clock,
    ledger::Ledger,
    messages::{ToolCall, Usage},
    remedy::{Limit, Remedies},
    tool::ToolOutput,
};

/// The share of a cap at which the model is nudged: half.
pub const NUDGE_SHARE: f64 = 0.5;

const CONTINUE: &str = "Post a message to continue; the turn caps reset each turn.";
const WAY_OUT: &str = "If you are making progress, carry on. If you are repeating yourself or waiting on something that will not change, stop: try a different approach, or end your turn and tell the human what is blocking you.";

struct Gauge {
    key: String,
    used: f64,
    cap: f64,
    /// Below this a nudge is not due (defaults to half the cap).
    nudge_at: Option<f64>,
    /// For the model, at the nudge.
    nudge: String,
    /// For the human, at the stop.
    stop: String,
    /// Which cap, for the host's remedy at the stop.
    limit: Limit,
}

/// Key-order-independent JSON, so `{a,b}` and `{b,a}` are the same call.
fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(o) => canonical_object(o, out),
        other => out.push_str(&other.to_string()),
    }
}

fn canonical_object(o: &Map<String, Value>, out: &mut String) {
    let mut keys: Vec<&String> = o.keys().collect();
    keys.sort();
    out.push('{');
    for (i, k) in keys.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(k).unwrap_or_default());
        out.push(':');
        canonical(&o[k], out);
    }
    out.push('}');
}

/// One call + its result, as a key. A 64-bit hash: it only has to tell apart the
/// handful of distinct calls one turn makes, and it keeps no result text alive.
fn signature(call: &ToolCall, out: &ToolOutput) -> u64 {
    let mut input = String::new();
    canonical_object(&call.input, &mut input);
    let mut h = DefaultHasher::new();
    (
        &call.name,
        '\0',
        input,
        '\0',
        out.is_error,
        '\0',
        &out.content,
    )
        .hash(&mut h);
    h.finish()
}

fn minutes(ms: f64) -> String {
    format!("{} min", (ms / 60_000.0).round())
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
    clock: Arc<dyn Clock>,
    ledger: Option<Arc<Mutex<Ledger>>>,
    remedies: Remedies,
    started: u64,
    steps: u64,
    /// In first-seen order: the order the nudges list them in.
    repeats: Vec<Repeat>,
    nudged: HashSet<String>,
    /// Floor of the NEXT request: the last one sent, plus what came back since.
    next_request_tokens: u64,
}

impl TurnGuard {
    /// A fresh guard for one turn; its time cap counts from now on `clock`.
    /// A stop's text ends with the host's remedy for its cap.
    pub(crate) fn new(
        clock: Arc<dyn Clock>,
        ledger: Option<Arc<Mutex<Ledger>>>,
        remedies: Remedies,
    ) -> Self {
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
        }
    }

    fn charge(&self, u: &Usage) {
        if let Some(l) = &self.ledger {
            l.lock().unwrap().charge(u);
        }
    }

    /// A model call came back; its bill counts toward the conversation too.
    pub(crate) fn called(&mut self, u: &Usage) {
        self.steps += 1;
        self.charge(u);
        self.next_request_tokens += u.output;
    }

    /// A tool answered: its own spend is charged, and an identical call+result is counted.
    pub(crate) fn answered(&mut self, call: &ToolCall, out: &ToolOutput) {
        if let Some(c) = out.cost_usd.filter(|c| *c != 0.0) {
            self.charge(&Usage {
                cost_usd: Some(c),
                ..Default::default()
            });
        }
        let key = signature(call, out);
        match self.repeats.iter_mut().find(|r| r.key == key) {
            Some(r) => r.count += 1,
            None => self.repeats.push(Repeat {
                key,
                name: call.name.clone(),
                count: 1,
            }),
        }
        self.next_request_tokens += (out.content.chars().count() as u64).div_ceil(4);
    }

    /// Why the turn must stop before the next model call, or None.
    pub(crate) fn stop(&self, usage: &Usage, caps: Option<&TurnCaps>) -> Option<String> {
        let ledger = self.ledger.as_ref();
        if let Some(why) = ledger.and_then(|l| l.lock().unwrap().exhausted()) {
            return Some(format!("Turn stopped: {why}"));
        }
        self.gauges(usage, caps)
            .into_iter()
            .find(|g| g.cap > 0.0 && g.used >= g.cap)
            .map(|g| {
                let text = format!("Turn stopped: {} {CONTINUE}", g.stop);
                self.remedies.add_to(text, g.limit)
            })
    }

    /// The nudge due now (each signal once per turn), or None.
    pub(crate) fn nudge(&mut self, usage: &Usage, caps: Option<&TurnCaps>) -> Option<String> {
        let due: Vec<Gauge> = self
            .gauges(usage, caps)
            .into_iter()
            .filter(|g| {
                g.cap > 0.0
                    && g.used >= g.nudge_at.unwrap_or(g.cap * NUDGE_SHARE)
                    && !self.nudged.contains(&g.key)
            })
            .collect();
        if due.is_empty() {
            return None;
        }
        let text: Vec<&str> = due.iter().map(|g| g.nudge.as_str()).collect();
        let text = format!("[haest guard] {} {WAY_OUT}", text.join(" "));
        self.nudged.extend(due.into_iter().map(|g| g.key));
        Some(text)
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
                key: "steps".into(),
                used: steps as f64,
                cap: l.max_turn_steps as f64,
                nudge_at: None,
                nudge: format!(
                    "You have made {steps} model calls this turn; the turn stops at {}.",
                    l.max_turn_steps
                ),
                stop: format!(
                    "it made {steps} model calls, the cap is {}.",
                    l.max_turn_steps
                ),
                limit: Limit::TurnSteps(l.max_turn_steps),
            },
            Gauge {
                key: "time".into(),
                used: elapsed,
                cap: time_cap,
                nudge_at: None,
                nudge: format!(
                    "This turn has run for {}; it stops at {}.",
                    minutes(elapsed),
                    minutes(time_cap)
                ),
                stop: format!(
                    "it ran for {}, the cap is {}.",
                    minutes(elapsed),
                    minutes(time_cap)
                ),
                limit: Limit::TurnTime(l.max_turn_time),
            },
            Gauge {
                key: "usd".into(),
                used: usd,
                cap: l.max_turn_usd,
                nudge_at: None,
                nudge: format!(
                    "This turn has spent ${usd:.2}; it stops at ${}.",
                    l.max_turn_usd
                ),
                stop: format!("it spent ${usd:.2}, the cap is ${}.", l.max_turn_usd),
                limit: Limit::TurnUsd(l.max_turn_usd),
            },
            Gauge {
                key: "tokens".into(),
                used: tokens as f64,
                cap: l.max_turn_tokens as f64,
                nudge_at: None,
                nudge: format!(
                    "This turn has billed {tokens} tokens; it stops at {}.",
                    l.max_turn_tokens
                ),
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
        let nudge_at = (cap as f64 * NUDGE_SHARE).ceil().max(2.0);
        self.repeats
            .iter()
            .map(|Repeat { key, name, count }| Gauge {
                key: format!("repeat:{key:016x}"),
                used: *count as f64,
                cap: cap as f64,
                nudge_at: Some(nudge_at),
                nudge: format!("You have called {name} with identical input {count} times this turn and got the identical result every time -- you look stuck in a loop, and calling it again will not change the answer. At {cap} repeats the turn is stopped."),
                stop: format!("{name} was called {count} times with identical input and returned the identical result each time -- haest was stuck in a loop (the cap is {cap} repeats)."),
                limit: Limit::Repeats(cap),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
