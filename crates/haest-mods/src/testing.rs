//! Fakes for tests of mods, in this crate and in the crates that bridge
//! mods to another language or process (`haest-mods = { ..., features =
//! ["testing"] }` in dev-dependencies): a host that records what reached
//! the human ([`TestUi`]), a human at an approval prompt ([`Human`]), a tool
//! that records what it ran ([`Probe`]), and the decision cases every
//! bridge must answer the way [`Hooks`](crate::Hooks) does
//! ([`decision_cases`], [`effect`]).

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use haest_core::{
    Agent, AgentDeps, AgentHooksRef, AgentOptions, SharedDeps, ToolRef, Tools,
    testing::ScriptedProvider,
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
    workspace::{ShutWorkspace, WorkspaceMode},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    ask::{ApprovalRequest, Decision},
    decorate::ModTools,
    env::{ModEnv, ModUi},
    gate::{ApprovalAnswer, Prompter},
    hooks::ModHooks,
    registry::Mods,
};

mod cases;

pub use cases::{DecisionCase, decision_cases, effect, hooks_answering, once, user_text};

/// The working directory every fake here runs in.
pub const CWD: &str = "/p";

/// `m`, held until the guard drops. A fake's lock is held only to read or
/// add one value, so a poisoned one is a broken test.
fn held<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().expect("a test fake's lock poisoned")
}

/// A host that records every line and ask that reached the human, and
/// answers the asks as scripted.
#[derive(Debug, Default)]
pub struct TestUi {
    infos: Mutex<Vec<String>>,
    errors: Mutex<Vec<String>>,
    asks: Mutex<Vec<ApprovalRequest>>,
    answers: Mutex<VecDeque<Decision>>,
    /// Asks never answer: they end only when their call is interrupted.
    hold: bool,
}

impl TestUi {
    /// A host whose every ask is a deny.
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Answers asks in this order; out of answers = deny.
    pub fn answering(answers: impl IntoIterator<Item = Decision>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Default::default()
        })
    }

    /// Never answers an ask: it ends only when its call is interrupted.
    pub fn holding() -> Arc<Self> {
        Arc::new(Self {
            hold: true,
            ..Default::default()
        })
    }

    /// Every info line, in order.
    pub fn infos(&self) -> Vec<String> {
        held(&self.infos).clone()
    }

    /// Every error line, in order.
    pub fn errors(&self) -> Vec<String> {
        held(&self.errors).clone()
    }

    /// Every ask, in order.
    pub fn asks(&self) -> Vec<ApprovalRequest> {
        held(&self.asks).clone()
    }
}

#[async_trait]
impl ModUi for TestUi {
    async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        held(&self.asks).push(req);
        if self.hold {
            cancel.cancelled().await;
            return Decision::Deny;
        }
        let next = held(&self.answers).pop_front();
        next.unwrap_or(Decision::Deny)
    }
    fn info(&self, text: &str) {
        held(&self.infos).push(text.to_string());
    }
    fn error(&self, text: &str) {
        held(&self.errors).push(text.to_string());
    }
}

/// A human at the gate's prompt: answers in order (out of answers = never
/// answers), counting the prompts and the asks an earlier ALWAYS answered.
#[derive(Debug, Default)]
pub struct Human {
    answers: Mutex<VecDeque<ApprovalAnswer>>,
    prompts: AtomicUsize,
    receipts: AtomicUsize,
}

impl Human {
    /// A human who gives these answers, in order.
    pub fn answering(answers: impl IntoIterator<Item = ApprovalAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Default::default()
        })
    }

    /// How many prompts they saw.
    pub fn prompts(&self) -> usize {
        self.prompts.load(Ordering::SeqCst)
    }

    /// How many asks an earlier ALWAYS answered for them.
    pub fn receipts(&self) -> usize {
        self.receipts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Prompter for Human {
    async fn prompt(&self, _req: &ApprovalRequest) -> ApprovalAnswer {
        self.prompts.fetch_add(1, Ordering::SeqCst);
        let next = held(&self.answers).pop_front();
        match next {
            Some(a) => a,
            None => std::future::pending().await,
        }
    }
    fn auto_approved(&self, _req: &ApprovalRequest) {
        self.receipts.fetch_add(1, Ordering::SeqCst);
    }
}

/// The mods' environment: [`CWD`], `workspace`, `ui`.
pub fn env(ui: Arc<TestUi>, workspace: WorkspaceMode) -> ModEnv {
    ModEnv::new(CWD, workspace, ui)
}

/// A tool call's context in [`CWD`], with no files and no shell.
pub fn ctx() -> ToolContext {
    ToolContext::new(CWD, ShutWorkspace)
}

/// `v`'s fields as a tool's input (none unless `v` is an object).
pub fn input(v: Value) -> JsonMap {
    v.as_object().cloned().unwrap_or_default()
}

/// A tool that records every input it ran with and answers `<name>:<v>` in lower case.
#[derive(Debug)]
pub struct Probe {
    name: String,
    read_only: bool,
    ran: Mutex<Vec<JsonMap>>,
}

impl Probe {
    fn with(name: &str, read_only: bool) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            read_only,
            ran: Mutex::default(),
        })
    }

    /// A tool named `name` that changes things.
    pub fn new(name: &str) -> Arc<Self> {
        Self::with(name, false)
    }

    /// A read-only tool named `name`.
    pub fn read_only(name: &str) -> Arc<Self> {
        Self::with(name, true)
    }

    /// Every input it ran with, in order.
    pub fn ran(&self) -> Vec<JsonMap> {
        held(&self.ran).clone()
    }
}

#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        "probe".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    async fn execute(&self, input: JsonMap, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let v = input
            .get("v")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        held(&self.ran).push(input);
        Ok(ToolOutput::text(format!(
            "{}:{v}",
            self.name.to_lowercase()
        )))
    }
}

/// `tools` wrapped in `mods`' hooks, plus the mods' own tools.
pub fn wrapped<T: Into<ToolRef>>(mods: Mods, tools: impl IntoIterator<Item = T>) -> Tools {
    ModTools::wrap(&mods, tools.into_iter().collect())
}

/// An agent on `p` as a host puts mods in its loop: `tools` in [`ModTools`],
/// `host`'s hooks in [`ModHooks`], the model `scripted:m`, in [`CWD`].
pub fn looped(
    p: &ScriptedProvider,
    host: impl Into<AgentHooksRef>,
    mods: &Mods,
    tools: Tools,
) -> Agent {
    let shared = SharedDeps::new(p.resolver(), CWD, ShutWorkspace);
    let deps = AgentDeps::new(shared, ModTools::new(tools, mods.clone()), "")
        .with_hooks(ModHooks::new(host, mods.clone()));
    Agent::new(deps, AgentOptions::new("scripted:m"))
}

/// The names of `tools`, in order.
pub fn names(tools: &[ToolRef]) -> Vec<String> {
    tools.iter().map(|t| t.name().to_string()).collect()
}

/// The last of `tools` named `name`.
///
/// # Panics
///
/// None is.
pub fn find(tools: &[ToolRef], name: &str) -> ToolRef {
    tools
        .iter()
        .rev()
        .find(|t| t.name() == name)
        .cloned()
        .unwrap_or_else(|| panic!("no tool {name}"))
}
