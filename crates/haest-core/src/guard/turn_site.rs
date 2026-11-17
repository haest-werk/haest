//! The guard as the [`TurnSite`]: what a turn's meters read, its thresholds
//! checked at the step boundary and before every model call, and the live
//! wind-down watch its tools ask.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use super::TurnGuard;
use crate::{
    caps::TurnCaps,
    deps::Clock,
    ledger::Ledger,
    messages::Usage,
    remedy::Limit,
    threshold::{Action, Crossing, Meter, Reading, Scope, Threshold, TurnSite, Watch},
    tool::ToolOutput,
    wind_down::{WindDownWatch, WindingDown},
};

/// One turn's side of its thresholds.
pub(super) struct TurnThresholds {
    /// The agent's watch, across turns; a guard of its own unless the agent
    /// hands it one.
    watch: Arc<Mutex<Watch<TurnSite>>>,
    /// What the last model call sent: the context's size.
    pub(super) context: u64,
    /// The turn has said it is winding down: once a turn.
    winding: bool,
    /// The turn's spend as its tools see it: the loop's when the tools of the
    /// last answer started, plus what each of them spent since.
    spent: Arc<Mutex<Usage>>,
}

impl Default for TurnThresholds {
    fn default() -> Self {
        Self {
            watch: Arc::new(Mutex::new(Watch::default())),
            context: 0,
            winding: false,
            spent: Arc::default(),
        }
    }
}

/// A fired threshold as a durable run's record names it: its place in the
/// list, what it does and what it reads ("threshold 0: remind on steps").
fn record_key(index: usize, t: &dyn Threshold) -> String {
    format!("threshold {index}: {} on {}", t.action(), t.meter())
}

impl TurnThresholds {
    /// The thresholds that have fired, as a durable run records them.
    pub(super) fn fired_keys(&self) -> Vec<String> {
        let watch = self.watch.lock().unwrap();
        let fired = watch.fired().enumerate().filter(|(_, (_, fired))| *fired);
        fired.map(|(i, (t, _))| record_key(i, t.as_ref())).collect()
    }

    /// Takes the thresholds `keys` name as fired, a resumed turn's: none
    /// fires again before it would have, and a turn that said it is winding
    /// down does not say it twice. A key naming no threshold in this list
    /// (the host changed it) is skipped.
    pub(super) fn restore_fired(&mut self, keys: &[String]) {
        let mut watch = self.watch.lock().unwrap();
        let named: Vec<(usize, Action)> = watch
            .fired()
            .enumerate()
            .filter(|(i, (t, _))| keys.contains(&record_key(*i, t.as_ref())))
            .map(|(i, (t, _))| (i, t.action()))
            .collect();
        for (i, action) in named {
            watch.set_fired(i);
            self.winding |= action == Action::WindDown;
        }
    }

    /// A tool answered: its own spend, and a subagent's, count from now on.
    pub(super) fn answered(&self, out: &ToolOutput) {
        let paid = out.cost_usd.unwrap_or(0.0) + out.child_cost_usd.unwrap_or(0.0);
        if paid != 0.0 {
            let mut spent = self.spent.lock().unwrap();
            spent.cost_usd = Some(spent.cost_usd.unwrap_or(0.0) + paid);
        }
    }
}

/// A turn's meters at one moment.
struct TurnReadings {
    caps: Option<TurnCaps>,
    steps: u64,
    elapsed: Duration,
    usage: Usage,
    context: u64,
    /// The conversation's USD and unpriced tokens; None without a ledger.
    conversation: Option<[Reading; 2]>,
}

impl TurnReadings {
    fn read(&self, meter: Meter) -> Option<Reading> {
        let caps = self.caps.as_ref();
        let capped = |cap: f64| (cap > 0.0).then_some(cap);
        let turn = |value: f64, cap: Option<f64>, limit: Option<Limit>| Reading {
            meter,
            value,
            cap,
            limit,
        };
        Some(match meter {
            Meter::Steps => turn(
                self.steps as f64,
                caps.and_then(|c| capped(c.max_turn_steps as f64)),
                caps.map(TurnCaps::steps_limit),
            ),
            Meter::WallTime => turn(
                self.elapsed.as_secs_f64(),
                caps.and_then(|c| capped(c.max_turn_time.as_secs_f64())),
                caps.map(TurnCaps::time_limit),
            ),
            Meter::Usd(Scope::Turn) => turn(
                self.usage.cost_usd.unwrap_or(0.0),
                caps.and_then(|c| capped(c.max_turn_usd)),
                caps.map(TurnCaps::usd_limit),
            ),
            Meter::Tokens(Scope::Turn) => turn(
                self.usage.context_tokens() as f64,
                caps.and_then(|c| capped(c.max_turn_tokens as f64)),
                caps.map(|c| Limit::TurnTokens(c.max_turn_tokens)),
            ),
            Meter::ContextTokens => turn(
                self.context as f64,
                caps.and_then(|c| capped(c.context_window as f64)),
                None,
            ),
            Meter::Usd(Scope::Conversation) | Meter::Tokens(Scope::Conversation) => {
                return self.conversation?.into_iter().find(|r| r.meter == meter);
            }
            Meter::Turns => return None,
        })
    }
}

/// How long since `started` on `clock`.
fn since(clock: &dyn Clock, started: u64) -> Duration {
    Duration::from_millis(clock.now_ms().saturating_sub(started))
}

fn conversation(ledger: Option<&Ledger>) -> Option<[Reading; 2]> {
    ledger.map(Ledger::readings)
}

impl TurnGuard {
    /// This turn checks the agent's thresholds (`watch`); the context held
    /// `context` tokens when it started.
    pub(crate) fn with_thresholds(
        mut self,
        watch: Arc<Mutex<Watch<TurnSite>>>,
        context: u64,
    ) -> Self {
        self.turn.watch = watch;
        self.turn.context = context;
        self
    }

    fn readings(&self, usage: &Usage, caps: Option<&TurnCaps>) -> TurnReadings {
        TurnReadings {
            caps: caps.cloned(),
            steps: self.steps,
            elapsed: since(self.clock.as_ref(), self.started),
            usage: usage.clone(),
            context: self.turn.context,
            conversation: conversation(self.ledger.as_ref()),
        }
    }

    /// The thresholds that take `action` and fire now.
    fn crossed(&self, usage: &Usage, caps: Option<&TurnCaps>, action: Action) -> Vec<Crossing> {
        let readings = self.readings(usage, caps);
        let mut watch = self.turn.watch.lock().unwrap();
        watch.check(|m| readings.read(m), action)
    }

    /// The reminders due now: the texts, then each closing once.
    pub(super) fn reminders(
        &self,
        usage: &Usage,
        caps: Option<&TurnCaps>,
    ) -> (Vec<String>, Vec<String>) {
        let due = self.crossed(usage, caps, Action::Remind);
        let texts = due.iter().map(Crossing::text).collect();
        let mut closings: Vec<String> = Vec::new();
        for closing in due.iter().filter_map(|c| c.threshold.closing()) {
            if !closings.iter().any(|seen| seen == closing) {
                closings.push(closing.to_string());
            }
        }
        (texts, closings)
    }

    /// What the human must hear now, each with the host's remedy.
    pub(crate) fn warnings(&self, usage: &Usage, caps: Option<&TurnCaps>) -> Vec<String> {
        let due = self.crossed(usage, caps, Action::Warn);
        due.iter()
            .map(|c| c.remedied_text(&self.remedies))
            .collect()
    }

    /// Why a threshold stops the turn now, with the host's remedy; None to go on.
    pub(super) fn threshold_stop(&self, usage: &Usage, caps: Option<&TurnCaps>) -> Option<String> {
        let due = self.crossed(usage, caps, Action::Stop);
        let first = due.first()?;
        let text = format!("Turn stopped: {}", first.text());
        Some(first.reading.remedied(text, &self.remedies))
    }

    /// The wind-down notice due now: the first time this turn a wind-down
    /// threshold fires, then never again this turn.
    pub(crate) fn wind_down_notice(
        &mut self,
        usage: &Usage,
        caps: Option<&TurnCaps>,
    ) -> Option<String> {
        let due = self.crossed(usage, caps, Action::WindDown);
        let first = due.first()?;
        if std::mem::replace(&mut self.turn.winding, true) {
            return None;
        }
        Some(WindingDown::of(first, &self.remedies).notice())
    }

    /// What the tools of one model answer ask: `usage` is the turn's spend as
    /// they start, `caps` the turn's caps for them. Live: a wind-down
    /// threshold past its mark now, whether or not it fired.
    pub(crate) fn wind_down_watch(&self, usage: &Usage, caps: Option<TurnCaps>) -> WindDownWatch {
        let thresholds = self.turn.watch.lock().unwrap().thresholds().clone();
        if !thresholds.iter().any(|t| t.action() == Action::WindDown) {
            return WindDownWatch::default();
        }
        *self.turn.spent.lock().unwrap() = usage.clone();
        let spent = self.turn.spent.clone();
        let (clock, started, steps) = (self.clock.clone(), self.started, self.steps);
        let (ledger, remedies) = (self.ledger.clone(), self.remedies.clone());
        let context = self.turn.context;
        WindDownWatch::new(move || {
            let readings = TurnReadings {
                caps: caps.clone(),
                steps,
                elapsed: since(clock.as_ref(), started),
                usage: spent.lock().unwrap().clone(),
                context,
                conversation: conversation(ledger.as_ref()),
            };
            let past = thresholds.past(|m| readings.read(m), Action::WindDown)?;
            Some(WindingDown::of(&past, &remedies))
        })
    }
}
