//! A conversation for the hub tests: main and subagents answer from separate
//! scripts (a subagent is told apart by its system prompt), a host that records
//! what the hub tells it, and a main loop that runs what reaches its inbox.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use haest_core::{
    Agent, AgentDeps, AgentOptions, AssistantMessage, AssistantPart, LoopEvent, Message,
    StopReason, ToolCall, Usage,
    ledger::{ConversationCaps, Ledger, LedgerHooks, LedgerSnapshot},
    provider::{Effort, Provider, ProviderEvent, ProviderRequest, ProviderStream, ResolvedModel},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::super::{AgentRef, SubagentCaps, SubagentEvent, SubagentHub, SubagentParent};
use crate::{GlobTool, inbox::Inbox, testkit::TestWorkspace};

pub type Script =
    Box<dyn Fn(ProviderRequest, usize) -> BoxFuture<'static, Vec<AssistantPart>> + Send + Sync>;

/// A script that answers at once.
pub fn now(
    f: impl Fn(&ProviderRequest, usize) -> Vec<AssistantPart> + Send + Sync + 'static,
) -> Script {
    Box::new(move |req, n| {
        let parts = f(&req, n);
        Box::pin(async move { parts })
    })
}

pub fn say(text: &str) -> Vec<AssistantPart> {
    vec![AssistantPart::Text { text: text.into() }]
}

pub fn call(id: &str, name: &str, input: Value) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
    })
}

/// Main's script: one Agent call with `input`, then `then` on every later call.
pub fn spawns(input: Value, then: &'static str) -> Script {
    now(move |_, n| match n {
        0 => vec![call("a", "Agent", input.clone())],
        _ => say(then),
    })
}

/// Every call costs $0.01.
pub const CALL_USD: f64 = 0.01;

pub struct Roles {
    main: Script,
    sub: Script,
    calls: Mutex<(usize, usize)>,
    pub sub_tools: Mutex<Vec<Vec<String>>>,
    pub sub_systems: Mutex<Vec<String>>,
}

impl Provider for Roles {
    fn id(&self) -> &str {
        "fake"
    }
    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let is_sub = req.system.contains("# You are a subagent");
        let n = {
            let mut c = self.calls.lock().unwrap();
            let slot = if is_sub { &mut c.1 } else { &mut c.0 };
            *slot += 1;
            *slot - 1
        };
        if is_sub {
            let names = req.tools.iter().map(|t| t.name.clone()).collect();
            self.sub_tools.lock().unwrap().push(names);
            self.sub_systems.lock().unwrap().push(req.system.clone());
        }
        let (model, cancel) = (req.model.clone(), req.cancel.clone());
        let parts = (if is_sub { &self.sub } else { &self.main })(req, n);
        Box::pin(futures::stream::once(async move {
            let mut m = AssistantMessage::start("fake", &model);
            tokio::select! {
                content = parts => {
                    let tools = content.iter().any(|p| matches!(p, AssistantPart::ToolCall(_)));
                    m.stop_reason = if tools { StopReason::ToolUse } else { StopReason::EndTurn };
                    m.content = content;
                    m.usage = Usage { cost_usd: Some(CALL_USD), ..Default::default() };
                }
                _ = cancel.cancelled() => m.stop_reason = StopReason::Aborted,
            }
            ProviderEvent::Done(m)
        }))
    }
}

/// A tool only main may have.
struct TaskCreate;

#[async_trait]
impl Tool for TaskCreate {
    fn name(&self) -> &str {
        "TaskCreate"
    }
    fn description(&self) -> String {
        "x".into()
    }
    fn parameters(&self) -> Value {
        serde_json::json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "task.create"
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("ok"))
    }
}

struct Quiet;
impl LedgerHooks for Quiet {
    fn persist(&self, _: &LedgerSnapshot) {}
    fn warn(&self, _: &str) {}
}

pub struct FakeParent {
    pub provider: Arc<Roles>,
    ws: Arc<TestWorkspace>,
    pub ledger: Arc<Mutex<Ledger>>,
    tx: UnboundedSender<String>,
    pub activity: Mutex<Vec<usize>>,
    /// (agent id, event kind) in order.
    pub events: Mutex<Vec<(String, &'static str)>>,
}

impl Inbox for FakeParent {
    fn submit(&self, text: String) {
        let _ = self.tx.send(text);
    }
}

#[async_trait]
impl SubagentParent for FakeParent {
    fn deps(&self) -> AgentDeps {
        let p = self.provider.clone();
        let models = move |spec: &str| {
            let model = spec.split_once(':').map_or(spec, |(_, m)| m).to_string();
            Ok::<_, String>(ResolvedModel {
                provider: p.clone(),
                model,
            })
        };
        let mut d = AgentDeps::new(
            Arc::new(models),
            Arc::new(self.base_tools()),
            Arc::new("main".to_string()),
            "/p",
            self.ws.clone(),
        );
        d.ledger = Some(self.ledger.clone());
        d
    }
    fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(GlobTool), Arc::new(TaskCreate)]
    }
    async fn system_prompt(&self, model_spec: &str, tools: &[Arc<dyn Tool>]) -> String {
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        format!(
            "You are haest.\n- Model: {model_spec}\n- Tools: {}",
            names.join(", ")
        )
    }
    fn default_model(&self) -> (String, Option<Effort>) {
        ("fake:main-model".into(), None)
    }
    fn on_activity(&self, running: &[String]) {
        self.activity.lock().unwrap().push(running.len());
    }
    fn on_subagent(&self, id: &str, event: SubagentEvent) {
        let kind = match event {
            SubagentEvent::Started { .. } => "started",
            SubagentEvent::Loop(_) => "loop",
            SubagentEvent::InnerCall(_) => "inner",
            SubagentEvent::Stopped { .. } => "stopped",
        };
        self.events.lock().unwrap().push((id.to_string(), kind));
    }
}

pub struct Rig {
    pub hub: Arc<SubagentHub>,
    pub parent: Arc<FakeParent>,
    pub main: Agent,
    pub events: Vec<LoopEvent>,
    pub inbox: UnboundedReceiver<String>,
}

impl Rig {
    pub fn new(main: Script, sub: Script, caps: SubagentCaps) -> Self {
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
            ws: TestWorkspace::with_files([("/p/a.txt", "a")]),
            ledger: Arc::new(Mutex::new(Ledger::new(
                ConversationCaps::default(),
                Arc::new(Quiet),
            ))),
            tx,
            activity: Mutex::default(),
            events: Mutex::default(),
        });
        let hub = SubagentHub::new(parent.clone(), caps);
        let mut tools = parent.base_tools();
        tools.extend(hub.tools_for(&AgentRef::main()));
        let deps = AgentDeps {
            tools: Arc::new(tools),
            ..parent.deps()
        };
        let main = Agent::new(deps, AgentOptions::new("fake:main-model"));
        Self {
            hub,
            parent,
            main,
            events: Vec::new(),
            inbox,
        }
    }

    pub fn main_tools(&self) -> Vec<String> {
        self.main
            .deps()
            .tools
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    pub async fn turn(&mut self, text: &str) {
        self.main.run(text, &mut self.events).await;
    }

    /// Run whatever reached main's inbox as turns until `done` holds (5s at most).
    pub async fn settle(&mut self, done: impl Fn(&Rig) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(self) && Instant::now() < end {
            match self.inbox.try_recv() {
                Ok(text) => self.turn(&text).await,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    /// Main's user and tool-result texts, in order.
    pub fn texts(&self) -> Vec<String> {
        texts(self.main.history())
    }

    pub fn tool_results(&self) -> Vec<(String, bool)> {
        let results = self.main.history().iter().filter_map(|m| match m {
            Message::Tool(t) => Some((t.content.clone(), t.is_error)),
            _ => None,
        });
        results.collect()
    }

    pub fn ids(&self, kind: &str) -> Vec<String> {
        let events = self.parent.events.lock().unwrap();
        events
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

pub fn texts(messages: &[Message]) -> Vec<String> {
    let texts = messages.iter().filter_map(|m| match m {
        Message::User(u) => Some(u.content.clone()),
        Message::Tool(t) => Some(t.content.clone()),
        Message::Assistant(_) => None,
    });
    texts.collect()
}
