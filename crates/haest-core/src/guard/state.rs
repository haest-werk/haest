//! A turn guard as a durable run writes it down ([`TurnState`]), and back.

use super::{Repeat, TurnGuard};
use crate::{
    messages::Usage,
    run::{RepeatCount, TurnState},
};

impl TurnGuard {
    /// What the caps count now, with the turn's `usage`: a `turn` record.
    pub(crate) fn state(&self, usage: &Usage) -> TurnState {
        let repeats = self.nudged.iter().map(|key| format!("{REPEAT}{key:016x}"));
        let mut nudged: Vec<String> = repeats.chain(self.turn.fired_keys()).collect();
        nudged.sort();
        TurnState {
            usage: usage.clone(),
            steps: self.steps,
            elapsed_ms: self.clock.now_ms().saturating_sub(self.started),
            repeats: (self.repeats.iter())
                .map(|r| RepeatCount {
                    key: format!("{:016x}", r.key),
                    tool: r.name.clone(),
                    count: r.count,
                })
                .collect(),
            nudged,
            request_tokens: self.next_request_tokens,
        }
    }

    /// Go on from `state`: its model calls, repeats, nudges and fired
    /// thresholds count, and its time cap counts on from its elapsed time
    /// (the stop itself does not count).
    pub(crate) fn restore(&mut self, state: &TurnState) {
        self.steps = state.steps;
        self.started = self.clock.now_ms().saturating_sub(state.elapsed_ms);
        self.repeats = (state.repeats.iter())
            .filter_map(|r| {
                Some(Repeat {
                    key: u64::from_str_radix(&r.key, 16).ok()?,
                    name: r.tool.clone(),
                    count: r.count,
                })
            })
            .collect();
        let repeat = |key: &String| u64::from_str_radix(key.strip_prefix(REPEAT)?, 16).ok();
        self.nudged = state.nudged.iter().filter_map(repeat).collect();
        self.turn.restore_fired(&state.nudged);
        self.next_request_tokens = state.request_tokens;
    }
}

/// A nudged repeat's key in [`TurnState::nudged`]; the rest name the turn's
/// thresholds that fired.
const REPEAT: &str = "repeat:";

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use serde_json::json;

    use super::*;
    use crate::{caps::TurnCaps, messages::ToolCall, remedy::Remedies, tool::ToolOutput};

    fn guard_at(now: &Arc<AtomicU64>) -> TurnGuard {
        let now = now.clone();
        TurnGuard::new(
            move || now.load(Ordering::SeqCst),
            None,
            Remedies::default(),
        )
    }

    #[test]
    fn a_restored_guard_counts_on_from_where_the_state_left_off() {
        let caps = TurnCaps {
            max_turn_steps: 4,
            max_repeats: 2,
            ..TurnCaps::default()
        };
        let call = ToolCall {
            id: "c1".into(),
            name: "Echo".into(),
            input: json!({"v": 1}).as_object().unwrap().clone(),
        };
        let now = Arc::new(AtomicU64::new(1_000));
        let mut before = guard_at(&now);
        before.called(&Usage::default());
        before.called(&Usage::default());
        before.answered(&call, &ToolOutput::text("same"));
        let usage = Usage {
            input: 7,
            ..Usage::default()
        };
        assert!(before.nudge(&usage, Some(&caps)).is_some());
        now.store(1_600, Ordering::SeqCst);
        let state = before.state(&usage);

        // Another process, hours later on its clock.
        let later = Arc::new(AtomicU64::new(9_000_000));
        let mut after = guard_at(&later);
        after.restore(&state);

        assert_eq!(
            after.state(&usage),
            state,
            "the same counts, the same 600 ms"
        );
        // Already nudged at two model calls of four: not again.
        assert!(after.nudge(&usage, Some(&caps)).is_none());
        // One more identical call reaches the repeat cap of 2.
        after.answered(&call, &ToolOutput::text("same"));
        assert!(after.stop(&usage, Some(&caps)).is_some());
    }
}
