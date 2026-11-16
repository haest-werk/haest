//! [`start`] a session, and [`SessionHandle`], the local [`Backend`].

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use async_trait::async_trait;
use haest_core::TurnOutcome;
use haest_wire::{Answer, Snapshot, now_ms};
use tokio::sync::{mpsc, oneshot};

use crate::{
    Backend, BoxFuture, Control, Driven, History, Hub, Input, Meta, SessionError, Subscription,
    driver::{Command, Driver, Job},
};

/// Start a session over `driven`, publishing into `hub`: the driver runs on
/// its own tokio task from now on, and the handle drives it.
///
/// Needs a tokio runtime. The driver ends when [`Backend::close`] is called
/// or every handle is dropped; either way the conversation is closed
/// ([`Driven::close`]) and the hub with it.
pub fn start<D: Driven>(driven: D, hub: Hub) -> SessionHandle<D> {
    let (tx, rx) = mpsc::unbounded_channel();
    let meta = Meta {
        sid: hub.sid().to_string(),
        pid: std::process::id(),
        started_ms: now_ms(),
    };
    tokio::spawn(Driver::new(driven, hub.clone(), rx).run());
    SessionHandle { tx, hub, meta }
}

/// A running session: the local [`Backend`], plus what only the host that
/// started it may do ([`with`](Self::with), [`hold`](Self::hold)). Cheap to
/// clone; every clone drives the same session.
///
/// # Examples
///
/// A bare agent behind a session: a subscriber watches, a turn runs, a
/// second subscriber catches up from the ring.
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, ShutWorkspace, StopReason, Tool,
///     testing::{ScriptedProvider, text_answer},
/// };
/// use haest_session::{Backend, Body, Hub, HubOptions, Input, start};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let model = ScriptedProvider::new([text_answer("hello")]);
/// let tools: Vec<Arc<dyn Tool>> = Vec::new();
/// let deps = AgentDeps::new(
///     Arc::new(model.resolver()),
///     Arc::new(tools),
///     Arc::new(String::new()),
///     "/",
///     Arc::new(ShutWorkspace),
/// );
/// let agent = Agent::new(deps, AgentOptions::new("scripted:m"));
///
/// let hub = Hub::new("s1", HubOptions::default());
/// let session = start(agent, hub);
/// let end = session.turn(Input::human("say hello")).await.unwrap();
/// assert_eq!(end.stop_reason, StopReason::EndTurn);
///
/// // A peer that connects now gets the whole turn from the ring: the
/// // finished answer, its deltas folded into it.
/// let mut late = session.subscribe(0);
/// let mut said = String::new();
/// while let Some(frame) = late.recv().await {
///     match &frame.body {
///         Body::Loop(LoopEvent::TextDelta { .. }) => unreachable!("folded into the answer"),
///         Body::Loop(LoopEvent::Assistant { message }) => said = message.text(),
///         Body::Loop(LoopEvent::TurnEnd { .. }) => break,
///         _ => {}
///     }
/// }
/// assert_eq!(said, "hello");
/// session.close().await;
/// # }
/// ```
pub struct SessionHandle<D> {
    tx: mpsc::UnboundedSender<Command<D>>,
    hub: Hub,
    meta: Meta,
}

impl<D> Clone for SessionHandle<D> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            hub: self.hub.clone(),
            meta: self.meta.clone(),
        }
    }
}

/// While held, the session starts no turn ([`SessionHandle::hold`]); dropping
/// it lets go.
pub struct Hold {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl<D: Driven> SessionHandle<D> {
    /// The stream this session publishes into.
    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    fn send(&self, cmd: Command<D>) -> Result<(), SessionError> {
        self.tx.send(cmd).map_err(|_| SessionError::Closed)
    }

    /// Ask the driver; its reply, or Closed when it has ended.
    async fn ask<T>(
        &self,
        cmd: impl FnOnce(oneshot::Sender<T>) -> Command<D>,
    ) -> Result<T, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.send(cmd(tx))?;
        rx.await.map_err(|_| SessionError::Closed)
    }

    /// Run `f` on the conversation between turns (after the running one
    /// ends) and return what it gives: how a host makes a change the
    /// [`Backend`] has no word for (a terminal's slash commands).
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] once the session ended.
    pub async fn with<R, F>(&self, f: F) -> Result<R, SessionError>
    where
        R: Send + 'static,
        F: for<'a> FnOnce(&'a mut D) -> BoxFuture<'a, R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job<D> = job(move |driven| {
            Box::pin(async move {
                let _ = tx.send(f(driven).await);
            })
        });
        self.send(Command::With(job))?;
        rx.await.map_err(|_| SessionError::Closed)
    }

    /// Hold the session: once the running turn ends, no turn starts until
    /// the [`Hold`] is dropped; what arrives meanwhile waits, in order. How a
    /// terminal keeps its prompt free of a turn's output.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`].
    pub async fn hold(&self) -> Result<Hold, SessionError> {
        self.ask(Command::Hold).await?;
        let tx = self.tx.clone();
        Ok(Hold {
            release: Some(Box::new(move || drop(tx.send(Command::Release)))),
        })
    }

    /// Send `input` now; await the [`PendingTurn`] for the end of the turn that
    /// takes it. [`Backend::turn`] in two steps, for a host that sends before
    /// it lets go of a [`Hold`]: the input then waits for a turn of its own,
    /// after what already waited. (Sent while a turn runs, it is steered into
    /// that turn, as any input.)
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] once the session ended.
    pub fn send_input(&self, input: Input) -> Result<PendingTurn, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Submit {
            input,
            done: Some(tx),
        })?;
        Ok(PendingTurn(rx))
    }

    /// Resolves once no turn runs and none is about to start (or the session
    /// ended).
    pub async fn idle(&self) {
        let _ = self.ask(Command::Idle).await;
    }

    /// [`idle`](Self::idle), and nothing more is expected later (a background
    /// subagent's answer): what a run that answers once and exits waits for.
    pub async fn settled(&self) {
        let _ = self.ask(Command::Settled).await;
    }
}

/// The end of the turn that took an input ([`SessionHandle::send_input`]):
/// its outcome, or [`SessionError`] when it never ran (the gate held it, the
/// session closed).
#[derive(Debug)]
#[must_use = "dropping it does not withdraw the input, only the wait for its turn"]
pub struct PendingTurn(oneshot::Receiver<Result<TurnOutcome, SessionError>>);

impl Future for PendingTurn {
    type Output = Result<TurnOutcome, SessionError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0)
            .poll(cx)
            .map(|done| done.unwrap_or(Err(SessionError::Closed)))
    }
}

/// The higher-ranked closure, as a job: names the signature for the closure.
fn job<D, F>(f: F) -> Job<D>
where
    F: for<'a> FnOnce(&'a mut D) -> BoxFuture<'a, ()> + Send + 'static,
{
    Box::new(f)
}

#[async_trait]
impl<D: Driven> Backend for SessionHandle<D> {
    fn meta(&self) -> Meta {
        self.meta.clone()
    }

    async fn submit(&self, input: Input) -> Result<(), SessionError> {
        self.send(Command::Submit { input, done: None })
    }

    async fn turn(&self, input: Input) -> Result<TurnOutcome, SessionError> {
        self.send_input(input)?.await
    }

    async fn control(&self, control: Control) -> Result<(), SessionError> {
        self.ask(|reply| Command::Control { control, reply })
            .await?
    }

    async fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError> {
        self.hub.answer(ask, answer)
    }

    fn subscribe(&self, after: u64) -> Subscription {
        self.hub.subscribe(after)
    }

    async fn snapshot(&self) -> Result<Snapshot, SessionError> {
        self.ask(Command::Snapshot).await
    }

    async fn history(&self) -> Result<History, SessionError> {
        self.ask(Command::History).await
    }

    async fn close(&self) {
        let _ = self.ask(Command::Close).await;
    }
}
