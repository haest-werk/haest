//! What the crate's tests serve: a session over a scripted model, with a
//! tool that runs until it is let go or stopped; and a host's own backend
//! over one.

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    Agent, AgentDeps, AgentOptions, JsonMap, ShutWorkspace, Tool, ToolContext, ToolError,
    ToolOutput, ToolRef, Tools, TurnOutcome,
    testing::{ScriptedProvider, text_answer},
};
use haest_session::{
    Backend, BoxFuture, Control, History, Hub, Input, Meta, SessionError, SessionHandle,
    Subscription, start,
};
use haest_wire::{Answer, Snapshot};
use serde_json::{Value, json};
use tokio::sync::Notify;

/// A tool that answers once it is let go (or its call is stopped).
pub(crate) struct Wait(Arc<Notify>);

#[async_trait]
impl Tool for Wait {
    fn name(&self) -> &str {
        "Wait"
    }
    fn description(&self) -> String {
        "Waits until it is let go.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
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

/// A session over a scripted model, with the Wait tool.
pub(crate) fn session(model: &ScriptedProvider, hub: Hub) -> (SessionHandle<Agent>, Arc<Notify>) {
    let go = Arc::new(Notify::new());
    let tools: Tools = Tools::from_iter([ToolRef::new(Wait(go.clone()))]);
    let shared = haest_core::SharedDeps::new(model.resolver(), "/", ShutWorkspace);
    let deps = AgentDeps::new(shared, tools, "");
    let agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    (start(agent, hub), go)
}

pub(crate) fn hub() -> Hub {
    Hub::new("s1")
}

pub(crate) fn answers(texts: &[&str]) -> ScriptedProvider {
    ScriptedProvider::new(texts.iter().map(|t| text_answer(t)))
}

/// What a [`Hooked`] backend's `history` does, given the session.
type HistoryHook =
    dyn Fn(SessionHandle<Agent>) -> BoxFuture<'static, Result<History, SessionError>> + Send + Sync;

/// A host's own backend over a session: every call goes to the session but
/// `history`, which the test writes (to count the calls that wait, or to
/// break it), and, when the test breaks it, `subscribe`.
pub(crate) struct Hooked {
    inner: SessionHandle<Agent>,
    history: Box<HistoryHook>,
    /// `subscribe` panics for the frames a peer asks for; the transport's
    /// own wait for the end (from `u64::MAX`) still subscribes.
    subscribe_panics: bool,
}

impl Hooked {
    pub(crate) fn new(
        inner: SessionHandle<Agent>,
        history: impl Fn(SessionHandle<Agent>) -> BoxFuture<'static, Result<History, SessionError>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            inner,
            history: Box::new(history),
            subscribe_panics: false,
        }
    }

    /// A backend whose `history` panics: a host's own bug.
    pub(crate) fn panicking(inner: SessionHandle<Agent>) -> Self {
        Self::new(inner, |_| {
            Box::pin(async { panic!("the host's backend has a bug") })
        })
    }

    /// A backend whose `subscribe` panics for the frames a peer asks for: a
    /// host's own bug, met on the connection's own task.
    pub(crate) fn panicking_on_subscribe(inner: SessionHandle<Agent>) -> Self {
        Self {
            subscribe_panics: true,
            ..Self::new(inner, |inner| {
                Box::pin(async move { inner.history().await })
            })
        }
    }
}

#[async_trait]
impl Backend for Hooked {
    fn meta(&self) -> Meta {
        self.inner.meta().clone()
    }
    async fn submit(&self, input: Input) -> Result<(), SessionError> {
        self.inner.submit(input)
    }
    async fn turn(&self, input: Input) -> Result<TurnOutcome, SessionError> {
        self.inner.turn(input).await
    }
    async fn control(&self, control: Control) -> Result<(), SessionError> {
        self.inner.control(control).await
    }
    async fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError> {
        self.inner.answer(ask, answer)
    }
    fn subscribe(&self, after: u64) -> Subscription {
        if self.subscribe_panics && after != u64::MAX {
            panic!("the host's backend has a bug");
        }
        self.inner.subscribe(after)
    }
    async fn snapshot(&self) -> Result<Snapshot, SessionError> {
        self.inner.snapshot().await
    }
    async fn history(&self) -> Result<History, SessionError> {
        (self.history)(self.inner.clone()).await
    }
    async fn close(&self) {
        self.inner.close().await;
    }
}
