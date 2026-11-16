//! Fakes for this crate's tests: a host that records what reached the human,
//! a human at the gate's prompt, and a tool that records what it ran.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use kern_core::{
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
    workspace::{ShutWorkspace, WorkspaceMode},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    api::{ApprovalRequest, Decision, ModEnv, ModHost},
    decorate::ModTools,
    gate::{Answer, Prompter},
    js::{JsMod, Limits},
    registry::Mods,
};

pub(crate) const CWD: &str = "/p";

#[derive(Default)]
pub(crate) struct TestHost {
    notices: Mutex<Vec<String>>,
    errors: Mutex<Vec<String>>,
    asks: Mutex<Vec<ApprovalRequest>>,
    answers: Mutex<VecDeque<Decision>>,
    /// Asks never answer: they end only when their call is interrupted.
    hold: bool,
}

impl TestHost {
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
    pub fn holding() -> Arc<Self> {
        Arc::new(Self {
            hold: true,
            ..Default::default()
        })
    }
    pub fn notices(&self) -> Vec<String> {
        self.notices.lock().unwrap().clone()
    }
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }
    pub fn asks(&self) -> Vec<ApprovalRequest> {
        self.asks.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModHost for TestHost {
    async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        self.asks.lock().unwrap().push(req);
        if self.hold {
            cancel.cancelled().await;
            return Decision::Deny;
        }
        let next = self.answers.lock().unwrap().pop_front();
        next.unwrap_or(Decision::Deny)
    }
    fn notice(&self, text: &str) {
        self.notices.lock().unwrap().push(text.to_string());
    }
    fn error(&self, text: &str) {
        self.errors.lock().unwrap().push(text.to_string());
    }
}

/// A human at the gate's prompt: answers in order (out of answers = never
/// answers), counting the prompts and the asks an earlier ALWAYS answered.
#[derive(Default)]
pub(crate) struct Human {
    answers: Mutex<VecDeque<Answer>>,
    prompts: AtomicUsize,
    receipts: AtomicUsize,
}

impl Human {
    pub fn answering(answers: impl IntoIterator<Item = Answer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Default::default()
        })
    }
    pub fn prompts(&self) -> usize {
        self.prompts.load(Ordering::SeqCst)
    }
    pub fn receipts(&self) -> usize {
        self.receipts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Prompter for Arc<Human> {
    async fn prompt(&self, _req: &ApprovalRequest) -> Answer {
        self.prompts.fetch_add(1, Ordering::SeqCst);
        let next = self.answers.lock().unwrap().pop_front();
        match next {
            Some(a) => a,
            None => std::future::pending().await,
        }
    }
    fn auto_approved(&self, _req: &ApprovalRequest) {
        self.receipts.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) fn env(host: Arc<TestHost>, workspace: WorkspaceMode) -> ModEnv {
    ModEnv::new(CWD, workspace, host)
}

pub(crate) fn ctx() -> ToolContext {
    ToolContext::new(CWD, Arc::new(ShutWorkspace))
}

pub(crate) fn input(v: Value) -> JsonMap {
    v.as_object().cloned().unwrap_or_default()
}

/// A tool that records every input it ran with and answers `<name>:<v>` in lower case.
pub(crate) struct Probe {
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
    pub fn new(name: &str) -> Arc<Self> {
        Self::with(name, false)
    }
    pub fn read_only(name: &str) -> Arc<Self> {
        Self::with(name, true)
    }
    pub fn ran(&self) -> Vec<JsonMap> {
        self.ran.lock().unwrap().clone()
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
        self.ran.lock().unwrap().push(input);
        Ok(ToolOutput::text(format!(
            "{}:{v}",
            self.name.to_lowercase()
        )))
    }
}

/// Limits short enough that a runaway mod is stopped inside a test.
pub(crate) fn test_limits() -> Limits {
    Limits {
        slice: Duration::from_millis(300),
        ..Limits::default()
    }
}

/// Load `source` as the mod file `file`.
pub(crate) async fn js(file: &str, source: &str, kern: &ModEnv) -> Result<JsMod, String> {
    JsMod::load_with(Path::new(file), source.into(), kern, test_limits()).await
}

/// `tools` wrapped in `mods`' hooks, plus the mods' own tools.
pub(crate) fn wrapped(mods: Mods, tools: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
    ModTools::wrap(&Arc::new(mods), tools)
}

pub(crate) fn find(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools
        .iter()
        .rev()
        .find(|t| t.name() == name)
        .cloned()
        .unwrap_or_else(|| panic!("no tool {name}"))
}
