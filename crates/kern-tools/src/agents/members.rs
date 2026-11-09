//! The hub's table: every agent whose turn has not ended, and every name ever
//! used. A finished agent leaves (its history with it); only its name stays.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use kern_core::AgentControl;
use tokio_util::sync::CancellationToken;

use super::{AgentRef, MAIN, SpawnInput, SubagentCaps};

/// Unique per hub, so the same agent name in a revived host is a different id.
pub(super) fn run_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms:x}{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
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
    crate::slug::slug(text, 24, "agent")
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
    pub fn reachable(&self) -> String {
        let mut names = vec![MAIN.to_string()];
        names.extend(self.running().map(|m| m.me.name.clone()));
        names.join(", ")
    }

    /// Check the caps and take a unique name for a new agent of `from`.
    pub fn reserve(
        &mut self,
        from: &AgentRef,
        input: &SpawnInput,
        caps: &SubagentCaps,
        run_id: &str,
    ) -> Result<&Member, String> {
        if from.depth >= caps.max_depth {
            return Err(format!(
                "agents may nest {} deep (KERN_MAX_AGENT_DEPTH)",
                caps.max_depth
            ));
        }
        let running = self.running().count();
        if running >= caps.max_concurrent {
            return Err(format!(
                "{running} subagents are already running (KERN_MAX_SUBAGENTS is {}); wait for one, or StopAgent one",
                caps.max_concurrent
            ));
        }
        self.seq += 1;
        let seq = self.seq;
        let mut name = match &input.name {
            Some(n) => slug(n),
            None => format!("{}-{seq}", slug(&input.description)),
        };
        if name == MAIN || self.known(&name) {
            name = format!("{name}-{seq}");
        }
        self.live.push(Member {
            id: format!("kern-{name}-{run_id}"),
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
