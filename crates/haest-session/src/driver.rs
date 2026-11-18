//! The session driver: owns the [`Driven`] on its own task, runs one turn at
//! a time, makes changes between turns, and decides what runs next.

mod turn;

use std::{collections::VecDeque, panic::AssertUnwindSafe, time::Duration};

use futures::FutureExt as _;
use haest_core::{TurnOutcome, error::BoxError};
use haest_wire::{Origin, Outbound, Path, Snapshot, State};
use tokio::sync::{mpsc, oneshot};

use crate::{BoxFuture, Control, Driven, DrivenStatus, History, Hub, Input, SessionError};

/// How often the driver looks again while work it waits for is still out
/// (a background subagent's answer) and someone waits for it to settle.
const SETTLE_POLL: Duration = Duration::from_millis(250);

/// Work a host runs on the conversation between turns.
pub(crate) type Job<D> = Box<dyn for<'a> FnOnce(&'a mut D) -> BoxFuture<'a, ()> + Send>;

/// Where a turn's outcome goes, for the one who sent its input.
pub(crate) type Waiter = oneshot::Sender<Result<TurnOutcome, SessionError>>;

/// What a handle asks of the driver.
pub(crate) enum Command<D> {
    Submit {
        input: Input,
        done: Option<Waiter>,
    },
    Control {
        control: Control,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Snapshot(oneshot::Sender<Snapshot>),
    History(oneshot::Sender<History>),
    With(Job<D>),
    Hold(oneshot::Sender<()>),
    Release,
    Idle(oneshot::Sender<()>),
    Settled(oneshot::Sender<()>),
    Close(oneshot::Sender<()>),
}

/// Input steered into the last turn and not read by its end: it runs next.
struct Leftover {
    human: bool,
    waiters: Vec<Waiter>,
    /// How the turn it was steered into ended: theirs, if its text is gone.
    outcome: TurnOutcome,
}

pub(crate) struct Driver<D> {
    driven: D,
    hub: Hub,
    rx: mpsc::UnboundedReceiver<Command<D>>,
    /// Every handle is gone: nothing more can arrive.
    orphaned: bool,
    /// A close was asked: a host's job that has not started never will.
    closing: bool,
    /// Inputs that wait for a turn of their own.
    queue: VecDeque<(Input, Vec<Waiter>)>,
    /// Requests that wait for the running turn to end.
    deferred: VecDeque<Command<D>>,
    leftover: Option<Leftover>,
    /// Holds taken: no turn starts while any is.
    holds: usize,
    /// What the conversation said about itself after the last change.
    status: DrivenStatus,
    idle: Vec<oneshot::Sender<()>>,
    settled: Vec<oneshot::Sender<()>>,
}

/// A turn's input, and how it runs.
pub(super) enum TurnInput {
    /// A person's text as they sent it: through the [`Driven::rewriter`],
    /// then [`Driven::human_turn`].
    Human(String),
    /// A person's text the rewriter already ran on (steered into the last
    /// turn and left unread, with what arrived since): [`Driven::human_turn`].
    HumanRewritten(String),
    /// Ready as it is (through the gate already): [`Driven::run`].
    Ready(String),
}

/// Whether the driver goes on after a request.
enum Flow {
    Go,
    Stop(Option<oneshot::Sender<()>>),
}

impl<D: Driven> Driver<D> {
    pub(crate) fn new(driven: D, hub: Hub, rx: mpsc::UnboundedReceiver<Command<D>>) -> Self {
        let status = driven.status();
        Self {
            driven,
            hub,
            rx,
            orphaned: false,
            closing: false,
            queue: VecDeque::new(),
            deferred: VecDeque::new(),
            leftover: None,
            holds: 0,
            status,
            idle: Vec::new(),
            settled: Vec::new(),
        }
    }

    /// The driver's life, then its end. Code on this task that panics (a
    /// host's `with` job, a [`Driven`] method, the rewriter) ends it as a
    /// close does: what waits is refused, [`Driven::close`] gets its chance
    /// to stop what the conversation started and save, the stream ends; then
    /// the panic goes on. The stream ends however the task ends, a panic in
    /// `Driven::close` itself included.
    pub(crate) async fn run(mut self) {
        let _ends = EndsTheStream(self.hub.clone());
        match AssertUnwindSafe(self.serve()).catch_unwind().await {
            Ok(reply) => self.finish(reply).await,
            Err(panic) => {
                self.finish(None).await;
                std::panic::resume_unwind(panic);
            }
        }
    }

    /// Requests between turns, turns when there is one to run, and a wait
    /// for the next request or arrival, until a close (whose reply it
    /// returns) or every handle is gone.
    async fn serve(&mut self) -> Option<oneshot::Sender<()>> {
        self.publish_status(State::Idle);
        loop {
            while let Some(cmd) = self.deferred.pop_front() {
                if let Flow::Stop(reply) = self.between(cmd).await {
                    return reply;
                }
            }
            if self.orphaned {
                return None;
            }
            if self.holds == 0
                && let Some((input, waiters)) = self.next_turn()
            {
                self.turn(input, waiters).await;
                continue;
            }
            wake(&mut self.idle);
            if !self.driven.expecting() {
                wake(&mut self.settled);
            }
            let poll = !self.settled.is_empty();
            let free = self.holds == 0;
            let driven = &mut self.driven;
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    Some(cmd) => self.deferred.push_back(cmd),
                    None => self.orphaned = true,
                },
                // In a block, so `arrival` is called only when the branch is
                // polled: `select!` evaluates a disabled branch's expression.
                () = async { driven.arrival().await }, if free => {}
                () = tokio::time::sleep(SETTLE_POLL), if poll => {}
            }
        }
    }

    /// The next turn to run, and who waits for it: what the last turn left
    /// unread, then the next turn nobody asked for
    /// ([`Driven::next_agent_turn`]), then the queued inputs in order (a
    /// person's through `human_turn`, another program's through the gate).
    fn next_turn(&mut self) -> Option<(TurnInput, Vec<Waiter>)> {
        if let Some(left) = self.leftover.take() {
            match self.driven.take_unread() {
                Some(text) if left.human => {
                    return Some((TurnInput::HumanRewritten(text), left.waiters));
                }
                Some(text) => match self.driven.gate_agent(text) {
                    Some(text) => return Some((TurnInput::Ready(text), left.waiters)),
                    None => fail(left.waiters, || SessionError::Held),
                },
                // A request between turns took it: it went with that turn.
                None => {
                    for w in left.waiters {
                        let _ = w.send(Ok(left.outcome.clone()));
                    }
                }
            }
        }
        if let Some(text) = self.driven.next_agent_turn() {
            return Some((TurnInput::Ready(text), Vec::new()));
        }
        while let Some((input, waiters)) = self.queue.pop_front() {
            match input.origin {
                Origin::Human => return Some((TurnInput::Human(input.text), waiters)),
                Origin::Agent => match self.driven.gate_agent(input.text) {
                    Some(text) => return Some((TurnInput::Ready(text), waiters)),
                    None => fail(waiters, || SessionError::Held),
                },
            }
        }
        None
    }

    /// One request, between turns.
    async fn between(&mut self, cmd: Command<D>) -> Flow {
        match cmd {
            Command::Submit { input, done } => {
                self.queue.push_back((input, done.into_iter().collect()));
                if self.holds > 0 {
                    self.publish_status(State::Idle);
                }
            }
            Command::Control { control, reply } => {
                let done = self.change(control).await;
                // The status frame saying what changed goes out first.
                self.refresh();
                let _ = reply.send(done);
            }
            Command::Snapshot(tx) => {
                // A change nothing published yet (spend from a background
                // subagent) goes out first, so every peer hears of it.
                self.refresh();
                let _ = tx.send(self.snapshot(State::Idle));
            }
            Command::History(tx) => {
                let seq = self.hub.last_seq();
                let messages = self.driven.history();
                let _ = tx.send(History { seq, messages });
            }
            // After a close was asked it is dropped unstarted, and its
            // caller reads Closed: the close comes first.
            Command::With(job) if !self.closing => {
                self.job(job).await;
                self.refresh();
            }
            Command::With(_) => {}
            Command::Hold(tx) => {
                self.holds += 1;
                let _ = tx.send(());
            }
            Command::Release => self.holds = self.holds.saturating_sub(1),
            Command::Idle(tx) => self.idle.push(tx),
            Command::Settled(tx) => self.settled.push(tx),
            Command::Close(tx) => return Flow::Stop(Some(tx)),
        }
        Flow::Go
    }

    /// A host's job. What is asked meanwhile waits for its end, in order,
    /// but a snapshot is answered at once, as during a turn (a peer's
    /// `hello` never waits on host code), and a close stops the job: its
    /// future is dropped where it waits, and its caller reads Closed. (A job
    /// whose caller gave up stops itself:
    /// [`SessionHandle::with`](crate::SessionHandle::with).)
    async fn job(&mut self, job: Job<D>) {
        let mut run = job(&mut self.driven);
        loop {
            tokio::select! {
                biased;
                () = &mut run => return,
                cmd = self.rx.recv(), if !self.orphaned => match cmd {
                    // What the stream last said: what the job changes is
                    // published once it ends.
                    Some(Command::Snapshot(tx)) => {
                        let queued = queued(&self.queue, &self.deferred);
                        let now = snapshot(&self.status, State::Idle, queued, self.hub.last_seq());
                        let _ = tx.send(now);
                    }
                    Some(cmd) => {
                        let close = matches!(cmd, Command::Close(_));
                        self.deferred.push_back(cmd);
                        if close {
                            self.closing = true;
                            return;
                        }
                    }
                    // Every handle is gone, the job's caller with them: it
                    // has stopped itself.
                    None => self.orphaned = true,
                },
            }
        }
    }

    /// A change between turns; nothing changes when it fails.
    async fn change(&mut self, control: Control) -> Result<(), SessionError> {
        let failed = |what| move |source: BoxError| SessionError::Control { what, source };
        match control {
            Control::Interrupt => Ok(()),
            Control::SetModelSpec(spec) => self
                .driven
                .set_model_spec(&spec)
                .map_err(failed("set the model")),
            Control::SetEffort(effort) => {
                self.driven.set_effort(effort);
                Ok(())
            }
            Control::Clear => self.driven.clear().await.map_err(failed("clear")),
        }
    }

    /// The conversation may have changed: say so when it did.
    fn refresh(&mut self) {
        let now = self.driven.status();
        if now != self.status {
            self.status = now;
            self.publish_status(State::Idle);
        }
    }

    fn snapshot(&self, state: State) -> Snapshot {
        let queued = queued(&self.queue, &self.deferred);
        snapshot(&self.status, state, queued, self.hub.last_seq())
    }

    fn publish_status(&mut self, state: State) {
        let status = Outbound::Status(self.snapshot(state));
        self.hub.publish(Path::main(), status);
    }

    /// The end: what waits is refused, the conversation closes what it
    /// started and saves, the stream closes.
    async fn finish(&mut self, reply: Option<oneshot::Sender<()>>) {
        for (_, waiters) in self.queue.drain(..) {
            fail(waiters, || SessionError::Closed);
        }
        if let Some(left) = self.leftover.take() {
            fail(left.waiters, || SessionError::Closed);
        }
        self.driven.close().await;
        self.hub.close();
        wake(&mut self.idle);
        wake(&mut self.settled);
        if let Some(reply) = reply {
            let _ = reply.send(());
        }
        // Anything still asked gets its reply dropped: the handle reads Closed.
    }
}

/// Ends the stream when the driver ends, however it ends: unwinding from a
/// panic, or dropped with its task when the runtime shuts down. Every
/// subscription then ends, so transports and peers see the session go.
struct EndsTheStream(Hub);

impl Drop for EndsTheStream {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// The conversation now, as a peer reads it.
fn snapshot(status: &DrivenStatus, state: State, queued: usize, last_seq: u64) -> Snapshot {
    Snapshot {
        state,
        session: status.session.clone(),
        model_spec: status.model_spec.clone(),
        effort: status.effort.map(|e| e.as_str().to_string()),
        usd: status.usd,
        queued,
        messages: status.messages,
        last_seq,
    }
}

/// Inputs that wait for a turn of their own: those queued, and those sent
/// while a job ran, which wait for its end.
fn queued<D>(queue: &VecDeque<(Input, Vec<Waiter>)>, deferred: &VecDeque<Command<D>>) -> usize {
    let sent = deferred
        .iter()
        .filter(|cmd| matches!(cmd, Command::Submit { .. }));
    queue.len() + sent.count()
}

/// Everyone waiting here is told.
fn wake(waiting: &mut Vec<oneshot::Sender<()>>) {
    for w in waiting.drain(..) {
        let _ = w.send(());
    }
}

/// Every waiter gets `why`.
fn fail(waiters: Vec<Waiter>, why: impl Fn() -> SessionError) {
    for w in waiters {
        let _ = w.send(Err(why()));
    }
}
