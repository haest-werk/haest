//! A conversation for the hub tests: main and subagents answer from separate
//! scripts ([`super::script`]), a host that records what the hub tells it
//! ([`super::fake`]), and a main loop that runs what reaches its inbox.

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use haest_core::{
    Agent, AgentDeps, AgentOptions, LoopEvent, Message, SharedDeps,
    caps::ConversationCaps,
    ledger::{Ledger, NoLedgerHooks},
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

pub(super) use super::fake::{FakeParent, ForMain, Heard, Host, explore_or_general};
pub(super) use super::script::{CALL_USD, Script, call, now, say, spawns};
use super::{
    super::{
        AgentDirectory, AgentError, AgentMessage, AgentRef, Sender, SubagentCaps, SubagentHub,
        SubagentParent, SubagentTypes,
    },
    lifecycle::Lifecycle,
    script::Roles,
};
use crate::testkit::TestWorkspace;

pub(super) struct Rig {
    pub hub: SubagentHub,
    pub parent: Arc<FakeParent>,
    pub main: Agent,
    pub events: Vec<LoopEvent>,
    pub inbox: UnboundedReceiver<String>,
}

impl Rig {
    pub(super) fn new(main: Script, sub: Script, caps: SubagentCaps) -> Self {
        Self::with_host(main, sub, caps, Host::default())
    }

    /// A conversation whose host has `kinds` of subagent.
    pub(super) fn with_kinds(
        main: Script,
        sub: Script,
        caps: SubagentCaps,
        kinds: Option<SubagentTypes>,
    ) -> Self {
        let host = Host {
            kinds,
            ..Host::default()
        };
        Self::with_host(main, sub, caps, host)
    }

    /// A conversation whose host implements the member hooks (`lifecycle`).
    pub(super) fn with_lifecycle(
        main: Script,
        sub: Script,
        caps: SubagentCaps,
        lifecycle: Arc<Lifecycle>,
    ) -> Self {
        let host = Host {
            lifecycle: Some(lifecycle),
            ..Host::default()
        };
        Self::with_host(main, sub, caps, host)
    }

    /// A conversation whose host does what `host` says.
    pub(super) fn with_host(main: Script, sub: Script, caps: SubagentCaps, host: Host) -> Self {
        let provider = Arc::new(Roles {
            main,
            sub,
            calls: Mutex::default(),
            sub_tools: Mutex::default(),
            sub_systems: Mutex::default(),
        });
        let (tx, inbox) = unbounded_channel();
        let parent = Arc::new(FakeParent {
            provider,
            ws: TestWorkspace::from_files([("/p/a.txt", "a")]),
            ledger: Ledger::new(ConversationCaps::default(), NoLedgerHooks),
            tx,
            activity: Mutex::default(),
            activity_gate: Mutex::default(),
            events: Mutex::default(),
            heard: Mutex::default(),
            for_main: Mutex::default(),
            hub: OnceLock::new(),
            idle_at_answer: Mutex::default(),
            host,
            adjust: Mutex::default(),
        });
        let hub = SubagentHub::new(parent.clone(), caps);
        let _ = parent.hub.set(hub.downgrade());
        let mut tools = parent.base_tools();
        tools.extend(hub.tools_for(&AgentRef::main()));
        let deps = AgentDeps::new(parent.deps(), tools, "main");
        let main = Agent::new(deps, AgentOptions::new("fake:main-model"));
        Self {
            hub,
            parent,
            main,
            events: Vec::new(),
            inbox,
        }
    }

    /// Change the conversation's deps: main's from now on, and every
    /// subagent's when it starts.
    pub(super) fn adjust(&mut self, f: impl Fn(SharedDeps) -> SharedDeps + Send + Sync + 'static) {
        *self.parent.adjust.lock().unwrap() = Some(Box::new(f));
        let tools = self.main.deps().tools().clone();
        let deps = AgentDeps::new(self.parent.deps(), tools, "main");
        self.main = Agent::new(deps, AgentOptions::new("fake:main-model"));
    }

    /// The tool names each subagent's model calls offered, in call order.
    pub(super) fn provider_sub_tools(&self) -> Vec<Vec<String>> {
        self.parent.provider.sub_tools.lock().unwrap().clone()
    }

    pub(super) fn main_tools(&self) -> Vec<String> {
        self.main
            .deps()
            .tools()
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    pub(super) async fn turn(&mut self, text: &str) {
        let _ = self.main.run(text, &mut self.events).await;
    }

    /// Run whatever reached main's inbox as turns until `done` holds (5s at most).
    pub(super) async fn settle(&mut self, done: impl Fn(&Rig) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(self) && Instant::now() < end {
            match self.inbox.try_recv() {
                Ok(text) => self.turn(&text).await,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    /// Main's user and tool-result texts, in order.
    pub(super) fn texts(&self) -> Vec<String> {
        texts(self.main.history())
    }

    pub(super) fn tool_results(&self) -> Vec<(String, bool)> {
        let results = self.main.history().iter().filter_map(|m| match m {
            Message::Tool(t) => Some((t.content.clone(), t.is_error)),
            _ => None,
        });
        results.collect()
    }

    pub(super) fn ids(&self, kind: &str) -> Vec<String> {
        let events = self.parent.events.lock().unwrap();
        events
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Every message the parent heard of.
    pub(super) fn heard(&self) -> Vec<Heard> {
        self.parent.heard.lock().unwrap().clone()
    }

    /// What reached main's inbox, and how.
    pub(super) fn for_main(&self) -> Vec<ForMain> {
        self.parent.for_main.lock().unwrap().clone()
    }

    /// Hand `text` from `from` to the agent `to`, as the hub delivers it.
    pub(super) fn deliver(&self, from: Sender, to: &str, text: &str) -> Result<String, AgentError> {
        self.hub.deliver(AgentMessage::new(from, to, text))
    }
}

/// The agent `name`, as the sender of a message (SendMessage).
pub(super) fn agent(name: &str) -> Sender {
    Sender::Agent(name.into())
}

pub(super) fn texts(messages: &[Message]) -> Vec<String> {
    let texts = messages.iter().filter_map(|m| match m {
        Message::User(u) => Some(u.content.clone()),
        Message::Tool(t) => Some(t.content.clone()),
        Message::Assistant(_) | Message::ToolChange(_) => None,
    });
    texts.collect()
}
