//! What the session tests drive: an agent over a scripted model, a tool that
//! waits until it is let go, a model call that waits until it is stopped, and
//! a conversation that records what the driver asked of it.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haest_core::{
    Agent, AgentControl, AgentDeps, AgentOptions, AssistantMessage, Effort, EventSink, JsonMap,
    LoopEvent, Message, ModelError, Provider, ProviderEvent, ProviderRequest, ResolvedModel,
    ShutWorkspace, StopReason, Tool, ToolContext, ToolError, ToolOutput, Tools, TurnOutcome,
    error::BoxError,
    provider::ProviderStream,
    testing::{ScriptedProvider, text_answer},
};
use tokio::sync::Notify;

use crate::{Body, Driven, DrivenStatus, Frame, RewritePrompt, RewritePromptRef, Subscription};

/// A tool that answers once it is let go (or its call is stopped).
pub(super) struct Wait(pub Arc<Notify>);

#[async_trait]
impl Tool for Wait {
    fn name(&self) -> &str {
        "Wait"
    }
    fn description(&self) -> String {
        "Waits until it is let go.".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "wait"
    }
    async fn execute(&self, _: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        tokio::select! {
            _ = self.0.notified() => Ok(ToolOutput::text("waited")),
            _ = ctx.cancel().cancelled() => Ok(ToolOutput::text("stopped")),
        }
    }
}

/// A model whose first call waits until the turn is stopped, then answers
/// from the script.
pub(super) struct Stall {
    calls: AtomicUsize,
    then: ScriptedProvider,
}

impl Stall {
    pub(super) fn new(then: ScriptedProvider) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            then,
        })
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Provider for Stall {
    fn id(&self) -> &str {
        "stall"
    }
    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        if self.calls.fetch_add(1, Ordering::Relaxed) > 0 {
            return self.then.stream(req);
        }
        let model = req.model.clone();
        Box::pin(stream::once(async move {
            req.cancel.cancelled().await;
            let mut m = AssistantMessage::start("stall", &model);
            m.stop_reason = StopReason::Aborted;
            ProviderEvent::Done(m)
        }))
    }
}

fn deps(models: impl Into<haest_core::ModelResolverRef>, tools: Tools) -> AgentDeps {
    AgentDeps::new(
        haest_core::SharedDeps::new(models, "/", ShutWorkspace),
        tools,
        "",
    )
}

/// An agent over `model`, with `tools`.
pub(super) fn agent(model: &ScriptedProvider, tools: Tools) -> Agent {
    Agent::new(
        deps(model.resolver(), tools),
        AgentOptions::new("scripted:m"),
    )
}

/// An agent whose first model call stalls until stopped.
pub(super) fn stalling_agent(stall: &Arc<Stall>) -> Agent {
    let p = stall.clone();
    let resolve = move |spec: &str| -> Result<ResolvedModel, ModelError> {
        let model = spec.split_once(':').map_or(spec, |(_, m)| m).to_string();
        Ok(ResolvedModel::new(p.clone(), model))
    };
    Agent::new(deps(resolve, Tools::new()), AgentOptions::new("stall:m"))
}

/// A text answer per text.
pub(super) fn answers(texts: &[&str]) -> ScriptedProvider {
    ScriptedProvider::new(texts.iter().map(|t| text_answer(t)))
}

/// What the driver asked of a [`Recorded`] conversation.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Asked {
    Human(String),
    Gate(String),
    Run(String),
    Close,
}

/// A rewriter that puts `[R] ` in front of a person's text.
pub(super) struct Marks;

#[async_trait]
impl RewritePrompt for Marks {
    async fn rewrite_prompt(&self, text: &str) -> String {
        format!("[R] {text}")
    }
}

/// A rewriter with a bug: it panics.
pub(super) struct Panics;

#[async_trait]
impl RewritePrompt for Panics {
    async fn rewrite_prompt(&self, _text: &str) -> String {
        panic!("the rewriter has a bug")
    }
}

/// An agent that records what the driver asks of it, with a gate that lets
/// `gate_passes` turns nobody asked for through and holds the rest.
pub(super) struct Recorded {
    pub agent: Agent,
    pub log: Arc<Mutex<Vec<Asked>>>,
    pub gate_passes: usize,
    pub rewriter: Option<RewritePromptRef>,
    /// Its `close` panics, after it is recorded.
    pub close_panics: bool,
}

impl Recorded {
    pub(super) fn new(agent: Agent, gate_passes: usize) -> Self {
        Self {
            agent,
            log: Arc::default(),
            gate_passes,
            rewriter: None,
            close_panics: false,
        }
    }

    fn note(&self, asked: Asked) {
        self.log.lock().unwrap().push(asked);
    }
}

#[async_trait]
impl Driven for Recorded {
    fn control(&self) -> AgentControl {
        self.agent.control()
    }
    async fn run(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        self.note(Asked::Run(input.to_string()));
        self.agent.run(input, sink).await
    }
    fn rewriter(&self) -> Option<RewritePromptRef> {
        self.rewriter.clone()
    }
    async fn human_turn(&mut self, text: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        self.note(Asked::Human(text.to_string()));
        self.run(text, sink).await
    }
    fn next_agent_turn(&mut self) -> Option<String> {
        let text = self.agent.control().take_queued()?;
        self.gate_agent(text)
    }
    fn gate_agent(&mut self, text: String) -> Option<String> {
        self.note(Asked::Gate(text.clone()));
        let passed = (self.log.lock().unwrap().iter())
            .filter(|a| matches!(a, Asked::Gate(_)))
            .count();
        (passed <= self.gate_passes).then_some(text)
    }
    fn history(&self) -> Vec<Message> {
        self.agent.history().to_vec()
    }
    fn status(&self) -> DrivenStatus {
        Driven::status(&self.agent)
    }
    fn set_model_spec(&mut self, spec: &str) -> Result<(), BoxError> {
        Driven::set_model_spec(&mut self.agent, spec)
    }
    fn set_effort(&mut self, effort: Option<Effort>) {
        self.agent.set_effort(effort);
    }
    async fn clear(&mut self) -> Result<(), BoxError> {
        self.agent.replace_history(Vec::new());
        Ok(())
    }
    async fn close(&mut self) {
        self.note(Asked::Close);
        assert!(!self.close_panics, "close has a bug");
    }
}

/// Frames until (and with) the first that `stop` picks; panics after 5 s.
pub(super) async fn until(
    sub: &mut Subscription,
    stop: impl Fn(&Frame) -> bool,
) -> Vec<Arc<Frame>> {
    let mut seen = Vec::new();
    let read = async {
        while let Some(frame) = sub.recv().await {
            let done = stop(&frame);
            seen.push(frame);
            if done {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("the frame never came");
    seen
}

/// Whether a frame is the conversation's own turn end.
pub(super) fn turn_end(frame: &Frame) -> bool {
    frame.path.is_main() && matches!(frame.body, Body::Loop(LoopEvent::TurnEnd { .. }))
}

/// What `fut` gives; panics, naming the caller's line, when it has not
/// answered within 5 s.
#[track_caller]
pub(super) fn within<F: IntoFuture>(fut: F) -> impl Future<Output = F::Output> {
    let at = std::panic::Location::caller();
    async move {
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .unwrap_or_else(|_| panic!("no answer within 5 s at {at}"))
    }
}

/// Polls until `cond` holds; panics after 5 s.
pub(super) async fn eventually(cond: impl Fn() -> bool) {
    let wait = async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .expect("it never happened");
}
