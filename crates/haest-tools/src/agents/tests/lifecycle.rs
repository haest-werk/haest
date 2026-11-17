//! A parent that implements the member hooks: it logs every start and end
//! with how many subagent model calls had gone out by then, may refuse a
//! start, and may run a member on a model, caps or deps of its own.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{AgentHooks, Message, SharedDeps, caps::ConversationCaps, ledger::Ledger};

use super::{script::Roles, support::Quiet};
use crate::agents::{AgentError, MemberCaps, MemberEnd, MemberRun, MemberStart, SubagentParent};

/// One step of a member's life, with the subagent model calls made before it.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Start {
        start: MemberStart,
        calls_before: usize,
    },
    End {
        name: String,
        end: MemberEnd,
        calls_before: usize,
    },
}

/// What the parent does at a start.
#[derive(Default)]
pub struct Lifecycle {
    /// Refuse a start whose description contains this.
    pub refuse: Option<&'static str>,
    /// The model spec its run names.
    pub model_spec: Option<String>,
    /// The caps its run names.
    pub caps: Option<MemberCaps>,
    /// Run it on deps of its own: hooks that keep its messages, its own ledger.
    pub own_deps: bool,
    /// Start only once this is notified (the run takes a while to start).
    pub wait: Option<Arc<tokio::sync::Notify>>,
    pub log: Mutex<Vec<Step>>,
    /// What the own deps' hooks kept, and the own ledger.
    pub kept: Arc<Kept>,
}

#[derive(Default)]
pub struct Kept {
    pub messages: Mutex<Vec<Message>>,
    pub ledger: Mutex<Option<Arc<Mutex<Ledger>>>>,
}

impl AgentHooks for Kept {
    fn on_message(&self, msg: &Message) {
        self.messages.lock().unwrap().push(msg.clone());
    }
}

impl Lifecycle {
    pub fn steps(&self) -> Vec<Step> {
        self.log.lock().unwrap().clone()
    }

    pub fn ends(&self) -> Vec<MemberEnd> {
        let steps = self.steps();
        let ends = steps.into_iter().filter_map(|s| match s {
            Step::End { end, .. } => Some(end),
            Step::Start { .. } => None,
        });
        ends.collect()
    }

    pub async fn start(
        self: &Arc<Self>,
        start: &MemberStart,
        provider: &Arc<Roles>,
        parent: &dyn SubagentParent,
    ) -> Result<Box<dyn MemberRun>, AgentError> {
        let calls_before = provider.sub_tools.lock().unwrap().len();
        self.log.lock().unwrap().push(Step::Start {
            start: start.clone(),
            calls_before,
        });
        if let Some(wait) = &self.wait {
            wait.notified().await;
        }
        if let Some(word) = self.refuse
            && start.input.description.contains(word)
        {
            return Err(AgentError::Refused(format!("no {word} here")));
        }
        let deps = self.own_deps.then(|| {
            let ledger = Arc::new(Mutex::new(Ledger::new(
                ConversationCaps::default(),
                Arc::new(Quiet),
            )));
            *self.kept.ledger.lock().unwrap() = Some(ledger.clone());
            parent
                .deps()
                .with_hooks(self.kept.clone())
                .with_ledger(ledger)
        });
        Ok(Box::new(Watched {
            name: start.name.clone(),
            lifecycle: self.clone(),
            provider: provider.clone(),
            deps,
        }))
    }
}

/// A member's run as the parent keeps it.
struct Watched {
    name: String,
    lifecycle: Arc<Lifecycle>,
    provider: Arc<Roles>,
    deps: Option<SharedDeps>,
}

#[async_trait]
impl MemberRun for Watched {
    fn model_spec(&self) -> Option<String> {
        self.lifecycle.model_spec.clone()
    }
    fn caps(&self) -> Option<MemberCaps> {
        self.lifecycle.caps
    }
    fn deps(&self) -> Option<SharedDeps> {
        self.deps.clone()
    }
    async fn end(self: Box<Self>, end: MemberEnd) {
        let calls_before = self.provider.sub_tools.lock().unwrap().len();
        self.lifecycle.log.lock().unwrap().push(Step::End {
            name: self.name.clone(),
            end,
            calls_before,
        });
    }
}
