//! The hub's table: every agent whose turn has not ended, and every name ever
//! used. A finished agent leaves (its history with it); only its name stays.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use haest_core::{AgentControl, now_ms};
use tokio_util::sync::CancellationToken;

use super::{AgentError, AgentRef, MAIN_AGENT, SpawnInput, SubagentCaps};

/// Unique per hub, so the same agent name in a revived host is a different id.
pub(super) fn run_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("{:x}{:x}", now_ms(), SEQ.fetch_add(1, Ordering::Relaxed))
}

/// One background answer owed ([`super::SubagentHub::answers_due`]); paid
/// when dropped, so a run that ends any way still settles.
pub(super) struct Due(Arc<AtomicUsize>);

impl Due {
    pub fn owe(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}

impl Drop for Due {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Running,
    Done,
    Stopped,
}

pub(super) struct Member {
    pub me: AgentRef,
    pub id: String,
    pub status: Status,
    /// Stops its turn (StopAgent).
    pub stop: CancellationToken,
    /// None until its agent is built.
    control: Option<AgentControl>,
    /// Messages that arrived before its agent existed: steered in once it does.
    pending: Vec<String>,
}

impl Member {
    pub fn steer(&mut self, text: String) {
        match &self.control {
            Some(c) => c.steer(text),
            None => self.pending.push(text),
        }
    }

    pub fn abort(&self) {
        self.stop.cancel();
        if let Some(c) = &self.control {
            c.abort();
        }
    }
}

/// An agent's address from free text: at most 24 characters.
fn slug(text: &str) -> String {
    haest_core::tool::slug(text, 24, "agent")
}

#[derive(Default)]
pub(super) struct Members {
    live: Vec<Member>,
    finished: HashSet<String>,
    seq: u64,
}

impl Members {
    pub fn running(&self) -> impl Iterator<Item = &Member> {
        self.live.iter().filter(|m| m.status == Status::Running)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Member> {
        self.live.iter_mut().find(|m| m.me.name == name)
    }

    /// Ever started here (running, or finished and gone).
    pub fn known(&self, name: &str) -> bool {
        self.finished.contains(name) || self.live.iter().any(|m| m.me.name == name)
    }

    /// Who a message can reach right now, for a refusal.
    pub fn reachable(&self) -> Vec<String> {
        let mut names = vec![MAIN_AGENT.to_string()];
        names.extend(self.running().map(|m| m.me.name.clone()));
        names
    }

    /// Check the caps and take a unique name for a new agent of `from`.
    pub fn reserve(
        &mut self,
        from: &AgentRef,
        input: &SpawnInput,
        caps: &SubagentCaps,
        run_id: &str,
    ) -> Result<&Member, AgentError> {
        if from.depth >= caps.max_depth {
            return Err(AgentError::TooDeep {
                max_depth: caps.max_depth,
            });
        }
        let running = self.running().count();
        if running >= caps.max_concurrent {
            return Err(AgentError::TooMany {
                running,
                max: caps.max_concurrent,
            });
        }
        self.seq += 1;
        let seq = self.seq;
        let mut name = match &input.name {
            Some(n) => slug(n),
            None => format!("{}-{seq}", slug(&input.description)),
        };
        if name == MAIN_AGENT || self.known(&name) {
            // `{name}-{seq}` may be taken too (an agent asked to be "x-3"): count on.
            let base = name;
            name = (seq..)
                .map(|n| format!("{base}-{n}"))
                .find(|n| !self.known(n))
                .expect("a u64 of suffixes outlasts the names ever used");
        }
        self.live.push(Member {
            id: format!("haest-{name}-{run_id}"),
            me: AgentRef {
                name,
                depth: from.depth + 1,
            },
            status: Status::Running,
            stop: CancellationToken::new(),
            control: None,
            pending: Vec::new(),
        });
        Ok(self.live.last().expect("just pushed"))
    }

    /// Its agent exists: what waited for it is steered in.
    pub fn attach(&mut self, name: &str, control: AgentControl) {
        if let Some(m) = self.get_mut(name) {
            for text in m.pending.drain(..) {
                control.steer(text);
            }
            m.control = Some(control);
        }
    }

    /// Its start was refused: it never ran, so it leaves the table AND its
    /// name is free again. Messages that waited for it go with it.
    pub fn forget(&mut self, name: &str) {
        self.live.retain(|m| m.me.name != name);
    }

    /// Its turn ended: it leaves the table, its name stays taken.
    pub fn leave(&mut self, name: &str) {
        if let Some(i) = self.live.iter().position(|m| m.me.name == name) {
            let mut m = self.live.remove(i);
            if m.status == Status::Running {
                m.status = Status::Done;
            }
            self.finished.insert(m.me.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> SpawnInput {
        SpawnInput {
            description: "work".into(),
            prompt: String::new(),
            name: Some(name.into()),
            model_spec: None,
            background: true,
            subagent_type: None,
        }
    }

    fn reserve(members: &mut Members, name: &str) -> String {
        let caps = SubagentCaps {
            max_concurrent: 16,
            ..SubagentCaps::default()
        };
        let m = members.reserve(&AgentRef::main(), &named(name), &caps, "r");
        m.unwrap().me.name.clone()
    }

    #[test]
    fn a_name_is_unique_even_when_the_suffix_it_would_get_was_asked_for() {
        let mut members = Members::default();
        // seq 1, 2, 3: the third "x" would become "x-3", which the first holds.
        let names: Vec<_> = ["x-3", "x", "x", "x-4", "x"]
            .map(|n| reserve(&mut members, n))
            .into();
        let unique: HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        // A finished agent's name stays taken too.
        members.leave("x-3");
        let again = reserve(&mut members, "x-3");
        assert!(!names.contains(&again), "{again} reused");
        // Each name reaches its own member: leaving one leaves exactly it.
        let live = |m: &Members| m.live.iter().map(|m| m.me.name.clone()).collect::<Vec<_>>();
        let before = live(&members);
        members.leave(&names[1]);
        assert_eq!(live(&members).len(), before.len() - 1);
        assert!(!live(&members).contains(&names[1]));
    }
}
