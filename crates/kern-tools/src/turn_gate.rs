//! THE PING-PONG GUARD. A turn another conversation or agent started -- or a
//! background job finishing -- is not a human deciding to spend. Two agents
//! that answer each other start turns forever, each one under the turn caps,
//! and so does a model that keeps starting jobs whose exit wakes it again -- so
//! the host counts the turns in a row with no human message between them. From
//! half the cap each turn's input carries a nudge with the count; at the cap
//! further messages are HELD -- kept, never dropped -- and ride in with the
//! human's next message. Ported from the TS kern's `host/agent-turns.ts`.
//!
//! The host runs what reaches its [`crate::Inbox`] through [`AgentTurnGate::agent_turn`],
//! and the human's own messages through [`AgentTurnGate::human`].

use kern_core::guard::NUDGE_SHARE;

pub struct AgentTurnGate {
    /// Turns in a row with no human message; 0 = no cap.
    cap: u32,
    /// The human must hear this (once per hold).
    notice: Box<dyn Fn(&str) + Send + Sync>,
    streak: u32,
    held: Vec<String>,
}

impl AgentTurnGate {
    pub fn new(cap: u32, notice: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            cap,
            notice: Box::new(notice),
            streak: 0,
            held: Vec::new(),
        }
    }

    /// The human posted: the streak ends, and anything held rides in ahead of their text.
    pub fn human(&mut self, text: &str) -> String {
        self.streak = 0;
        if self.held.is_empty() {
            return text.to_string();
        }
        let held = std::mem::take(&mut self.held).join("\n\n");
        format!(
            "[Messages from other conversations that kern held until the human posted:]\n\n{held}\n\n[The human's message:]\n\n{text}"
        )
    }

    /// A turn the human did not start. The text to run it with, or None: held.
    pub fn agent_turn(&mut self, text: &str) -> Option<String> {
        if self.cap > 0 && self.streak >= self.cap {
            if self.held.is_empty() {
                (self.notice)(&format!(
                    "kern is holding messages from other conversations and its own background jobs: {} turns in a row ran with no message from you (KERN_MAX_AGENT_TURNS is {}). They are kept, not dropped -- post anything and they are delivered with it.",
                    self.streak, self.cap
                ));
            }
            self.held.push(text.to_string());
            return None;
        }
        self.streak += 1;
        let nudge_from = (f64::from(self.cap) * NUDGE_SHARE).ceil() as u32;
        if self.cap == 0 || self.streak < nudge_from {
            return Some(text.to_string());
        }
        Some(format!(
            "[kern guard] This is turn {} in a row started by another conversation or a background job, with no message from the human since. If you are trading messages with another agent or restarting jobs, make sure it is still going somewhere; if it is not, stop. At {} kern holds further messages until the human posts.\n\n{text}",
            self.streak, self.cap
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    // Two agents answering each other start turns forever; the gate is what ends that.
    fn gate(cap: u32) -> (AgentTurnGate, Arc<Mutex<Vec<String>>>) {
        let notices = Arc::new(Mutex::new(Vec::new()));
        let into = notices.clone();
        let g = AgentTurnGate::new(cap, move |n| into.lock().unwrap().push(n.to_string()));
        (g, notices)
    }

    #[test]
    fn agent_turns_run_carry_a_nudge_from_half_the_cap_and_are_held_at_the_cap() {
        let (mut g, notices) = gate(4);
        assert_eq!(g.agent_turn("m1").as_deref(), Some("m1"));
        let nudged = g.agent_turn("m2").unwrap();
        assert!(nudged != "m2" && nudged.ends_with("m2"));
        assert!(g.agent_turn("m3").is_some());
        assert!(g.agent_turn("m4").is_some());
        assert_eq!(g.agent_turn("m5"), None);
        assert_eq!(g.agent_turn("m6"), None);
        assert_eq!(notices.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_human_posting_delivers_what_was_held_and_the_streak_starts_over() {
        let (mut g, _) = gate(1);
        g.agent_turn("m1");
        assert_eq!(g.agent_turn("held-a"), None);
        assert_eq!(g.agent_turn("held-b"), None);
        let delivered = g.human("hello");
        for part in ["held-a", "held-b", "hello"] {
            assert!(delivered.contains(part));
        }
        assert!(g.agent_turn("m2").is_some());
        assert_eq!(g.human("again"), "again");
    }

    #[test]
    fn cap_zero_never_holds() {
        let (mut g, _) = gate(0);
        for i in 0..100 {
            let m = format!("m{i}");
            assert_eq!(g.agent_turn(&m), Some(m.clone()));
        }
    }
}
