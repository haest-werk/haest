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
/// Needs a tokio runtime. The driver ends when
/// [`close`](SessionHandle::close) is called or every handle is dropped;
/// either way the conversation is closed ([`Driven::close`]) and the hub
/// with it.
///
/// Code that panics on the driver's task (a [`with`](SessionHandle::with)
/// job, a [`Driven`] method, the rewriter) ends the session the same way:
/// what waits reads [`SessionError::Closed`], `Driven::close` still runs,
/// the hub closes, and the panic then ends the task. The hub closes however
/// the task ends, a panic in `Driven::close` itself included.
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
/// started it may do ([`with`](Self::with), [`hold`](Self::hold)). Every
/// [`Backend`] method is also its own, so driving one needs no trait import.
/// Cheap to clone; every clone drives the same session, and a transport takes
/// a clone as it is.
///
/// # Examples
///
/// A bare agent behind a session: a subscriber watches, a turn runs, a
/// second subscriber catches up from the ring.
///
/// ```
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, SharedDeps, ShutWorkspace, StopReason, Tools,
///     testing::{ScriptedProvider, text_answer},
/// };
/// use haest_session::{Body, Hub, Input, start};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_session::SessionError> {
/// let model = ScriptedProvider::new([text_answer("hello")]);
/// let deps = AgentDeps::new(SharedDeps::new(model.resolver(), "/", ShutWorkspace), Tools::new(), "");
/// let session = start(Agent::new(deps, AgentOptions::new("scripted:m")), Hub::new("s1"));
///
/// let end = session.turn(Input::human("say hello")).await?;
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
/// # Ok(())
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

/// Who it is and whether it still runs; never the conversation it drives.
impl<D> std::fmt::Debug for SessionHandle<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("meta", &self.meta)
            .field("closed", &self.tx.is_closed())
            .finish_non_exhaustive()
    }
}

/// While held, the session starts no turn ([`SessionHandle::hold`]); dropping
/// it lets go.
#[must_use = "dropping it releases the hold"]
pub struct Hold {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

/// A live hold has nothing to show but itself.
impl std::fmt::Debug for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hold").finish_non_exhaustive()
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl<D: Driven> SessionHandle<D> {
    /// Who this is, for a peer's `hello`: the stream's id, the process, when
    /// the session started.
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The stream this session publishes into.
    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    /// Queue `input`: it joins a running turn at its next step, or runs as a
    /// turn of its own. Never waits.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] once the conversation ended.
    pub fn submit(&self, input: Input) -> Result<(), SessionError> {
        self.send(Command::Submit { input, done: None })
    }

    /// [`submit`](Self::submit), then wait for the end of the turn it
    /// entered history in.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`]; [`SessionError::Held`] when the turn gate
    /// held it.
    pub async fn turn(&self, input: Input) -> Result<TurnOutcome, SessionError> {
        self.send_input(input)?.await
    }

    /// Change the conversation. [`Control::Interrupt`] acts at once; the rest
    /// wait for the running turn to end.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`]; [`SessionError::Control`] when the change
    /// failed (nothing changed then).
    pub async fn control(&self, control: Control) -> Result<(), SessionError> {
        self.ask(|reply| Command::Control { control, reply })
            .await?
    }

    /// A peer's answer to an approval ask. Never waits.
    ///
    /// # Errors
    ///
    /// [`SessionError::NoSuchAsk`].
    pub fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError> {
        self.hub.answer(ask, answer)
    }

    /// The stream's frames after `after`, then every new one (see [`Subscription`]).
    pub fn subscribe(&self, after: u64) -> Subscription {
        self.hub.subscribe(after)
    }

    /// The conversation now.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`].
    pub async fn snapshot(&self) -> Result<Snapshot, SessionError> {
        self.ask(Command::Snapshot).await
    }

    /// The messages and the seq they are current to. Asked during a turn, it
    /// answers when that turn ends (the running turn's frames are in the ring).
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`].
    pub async fn history(&self) -> Result<History, SessionError> {
        self.ask(Command::History).await
    }

    /// End the conversation: the running turn is stopped, then everything it
    /// started is stopped and its session saved. Returns once that is done
    /// (at once when it has already ended).
    ///
    /// A [`with`](Self::with) job that has not ended is stopped first: the
    /// running one where it waits, and one waiting for its turn never
    /// starts. Their callers read [`SessionError::Closed`].
    pub async fn close(&self) {
        let _ = self.ask(Command::Close).await;
    }

    fn send(&self, cmd: Command<D>) -> Result<(), SessionError> {
        self.tx.send(cmd).map_err(|_closed| SessionError::Closed)
    }

    /// Ask the driver; its reply, or Closed when it has ended.
    async fn ask<T>(
        &self,
        cmd: impl FnOnce(oneshot::Sender<T>) -> Command<D>,
    ) -> Result<T, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.send(cmd(tx))?;
        rx.await.map_err(|_closed| SessionError::Closed)
    }

    /// Run `f` on the conversation between turns (after the running one
    /// ends) and return what it gives: how a host makes a change the
    /// [`Backend`] has no word for (a terminal's slash commands).
    ///
    /// `f` returns its future boxed, `|rt| Box::pin(async move { .. })`
    /// ([`BoxFuture`]): the future borrows the conversation and runs on the
    /// driver's task, so it must be `Send`, and an async closure
    /// (`AsyncFnOnce`) cannot promise that on stable Rust. A panic in it ends
    /// the session ([`start`] says how).
    ///
    /// No turn starts and no other request is served while `f` runs, so a
    /// host bounds a job that may wait for good (a mod's slash command) by
    /// giving up on it. Cancel-safe: dropped before it resolves (raced
    /// against Ctrl-C or a timeout), the job is withdrawn. One that has not
    /// started never runs; one that runs is stopped where it waits (its
    /// future is dropped), and the session goes on. [`close`](Self::close)
    /// stops it the same way.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] once the session ended, or when a close
    /// stopped the job.
    pub async fn with<R, F>(&self, f: F) -> Result<R, SessionError>
    where
        R: Send + 'static,
        F: for<'a> FnOnce(&'a mut D) -> BoxFuture<'a, R> + Send + 'static,
    {
        let (mut tx, rx) = oneshot::channel();
        let job: Job<D> = job(move |driven| {
            Box::pin(async move {
                if tx.is_closed() {
                    // Its caller gave up before it started.
                    return;
                }
                let done = {
                    let run = f(driven);
                    tokio::select! {
                        biased;
                        done = run => Some(done),
                        // Its caller gave up: `f`'s future is dropped here.
                        () = tx.closed() => None,
                    }
                };
                if let Some(done) = done {
                    let _ = tx.send(done);
                }
            })
        });
        self.send(Command::With(job))?;
        rx.await.map_err(|_closed| SessionError::Closed)
    }

    /// Hold the session: once the running turn ends, no turn starts until
    /// the [`Hold`] is dropped; what arrives meanwhile waits, in order. How a
    /// terminal keeps its prompt free of a turn's output.
    ///
    /// Cancel-safe: dropped before it resolves (it waits for a running turn
    /// to end), it holds nothing.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`].
    pub async fn hold(&self) -> Result<Hold, SessionError> {
        let tx = self.tx.clone();
        // Made before the hold is asked for, so a drop at the await below
        // releases it. The driver reads its requests in the order they were
        // sent, a hold it puts off until a turn ends included, so the
        // release always comes after the hold.
        let hold = Hold {
            release: Some(Box::new(move || drop(tx.send(Command::Release)))),
        };
        self.ask(Command::Hold).await?;
        Ok(hold)
    }

    /// Send `input` now; await the [`PendingTurn`] for the end of the turn that
    /// takes it. [`turn`](Self::turn) in two steps, for a host that sends before
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

/// Each method is the handle's own.
#[async_trait]
impl<D: Driven> Backend for SessionHandle<D> {
    fn meta(&self) -> Meta {
        SessionHandle::meta(self).clone()
    }

    async fn submit(&self, input: Input) -> Result<(), SessionError> {
        SessionHandle::submit(self, input)
    }

    async fn turn(&self, input: Input) -> Result<TurnOutcome, SessionError> {
        SessionHandle::turn(self, input).await
    }

    async fn control(&self, control: Control) -> Result<(), SessionError> {
        SessionHandle::control(self, control).await
    }

    async fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError> {
        SessionHandle::answer(self, ask, answer)
    }

    fn subscribe(&self, after: u64) -> Subscription {
        SessionHandle::subscribe(self, after)
    }

    async fn snapshot(&self) -> Result<Snapshot, SessionError> {
        SessionHandle::snapshot(self).await
    }

    async fn history(&self) -> Result<History, SessionError> {
        SessionHandle::history(self).await
    }

    async fn close(&self) {
        SessionHandle::close(self).await;
    }
}
