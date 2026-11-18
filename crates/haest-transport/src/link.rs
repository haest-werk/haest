//! One connection, whatever carries it ([`serve`]): `hello` first, then the
//! peer's requests served against the session, the stream's frames as it
//! asked for them, and a `reply` for every request. Every transport runs its
//! connections through here, so the rules are the same on each.

use std::{
    any::Any,
    fmt,
    future::Future,
    panic::{self, AssertUnwindSafe},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures::StreamExt;
use haest_core::{Effort, TurnOutcome, error::report};
use haest_session::{BackendRef, Body, Control, Frame, Input, SessionError, Subscription};
use haest_wire::{self as wire, Access, Envelope, Outbound, Request, Token, decode};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{JoinError, JoinHandle, JoinSet},
};

use crate::{Grant, TransportError};

/// Frames waiting for a slow peer before its subscription falls behind (and
/// gets a `gap`): the conversation never waits on a peer.
const OUT_BUFFER: usize = 1024;

/// How long a connection that ends gets to write what it still holds.
const FLUSH_GRACE: Duration = Duration::from_secs(1);

/// How long the writer sends only lines after a keepalive that resolved at
/// once ([`LineSink::keepalive`]): such a sink costs a keepalive this often,
/// never a writer that does nothing else.
const KEEPALIVE_REST: Duration = Duration::from_millis(100);

/// Requests of one connection the session may hold (a turn's end, a history
/// asked mid-turn): a request counts from its first poll to its answer, so
/// one answered at once never does. The next line is read only once the
/// request before it has been polled, so every request read before it
/// counts when a line is checked, in a burst written in one go too. At the
/// cap, a request that would wait too is refused, and one answered at once
/// ([`answered_at_once`]) is still served, so a busy peer keeps its stop
/// button. At twice the cap in flight, of any kind (those served at the
/// cap, replies a peer does not read), the connection reads no more lines
/// until one is answered, so a peer that only sends makes no more work than
/// that, and the rest wait in its pipe.
const MAX_IN_FLIGHT: usize = 32;

/// Lines a peer sends: one request each.
pub trait LineSource: Send {
    /// The next line; None when the peer is gone.
    ///
    /// [`serve`] calls it only when it takes a line: not while the backend
    /// call of the line before is still to start, nor with 64 requests in
    /// flight, and never again after None or an error. It keeps the future
    /// of each call until that future resolves, and drops one still pending
    /// only together with the source, when the connection ends. So a source
    /// needs no cancel-safety of its own: a read may take its line when it
    /// is called, or read it over several awaits.
    ///
    /// [`Peer`](crate::Peer) calls it once for each frame it reads, and
    /// drops the future only when its own caller drops that call (a `recv`
    /// raced against a timeout).
    fn next_line(&mut self) -> impl Future<Output = Option<Result<String, TransportError>>> + Send;
}

/// Where lines to the peer go: one frame each, without a line end.
pub trait LineSink: Send + 'static {
    /// Send one line.
    fn send_line(
        &mut self,
        line: String,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Keep the link alive: resolves once the sink sent something of its own
    /// (the broker link's ping when the broker has been quiet), Err when that
    /// failed, which ends the connection. By default it never resolves.
    ///
    /// The contract:
    ///
    /// - It waits for a reason to send (a timer, a quiet link). It is asked
    ///   again after every line and every keepalive, so one that is ready at
    ///   once would be all the writer does. The writer guards against that:
    ///   after a keepalive that never waited, it sends only lines for 100 ms
    ///   before it asks again. Such a sink still sends a keepalive that often.
    /// - It is cancel-safe: it is dropped at any await whenever a line comes
    ///   first, and starts over from the sink's state when asked again, so a
    ///   drop must leave no half-sent frame behind (a frame is handed to the
    ///   transport whole, or not at all).
    fn keepalive(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
        std::future::pending()
    }
}

/// How a connection starts.
#[derive(Debug, Clone, Default)]
pub struct Start {
    /// Follow the stream from after this seq at once, as if the peer had
    /// sent `events {after, follow: true}` (a parent on stdio follows from
    /// 0). None: the peer asks for what it wants.
    pub follow_from: Option<u64>,
    /// A credential for the `hello` (a broker reads it there, never in a
    /// URL). `Debug` never shows it.
    pub token: Option<Token>,
}

/// Why a connection ended. More reasons may come: a `match` has a `_` arm.
#[derive(Debug)]
#[non_exhaustive]
pub enum Ended {
    /// The peer is gone: its input ended, or it stopped taking frames.
    PeerLeft,
    /// The conversation ended (a `close` request, or the host closed it).
    Closed,
    /// Reading from the peer failed, or a broker refused the token
    /// ([`TransportError::Auth`]).
    Failed(TransportError),
}

/// For a person: `the peer left`, `the conversation ended`, `reading from
/// the peer failed: ` and the error with its causes, or, for a refused
/// token, the broker's refusal as it is.
impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ended::PeerLeft => f.write_str("the peer left"),
            Ended::Closed => f.write_str("the conversation ended"),
            Ended::Failed(e @ TransportError::Auth { .. }) => f.write_str(&report(e)),
            Ended::Failed(e) => write!(f, "reading from the peer failed: {}", report(e)),
        }
    }
}

/// A line out: a stream frame as it is, or a frame of this connection's own
/// (stamped with the last stream seq it was sent).
enum Out {
    Stream(Arc<Frame>),
    Conn(Outbound),
}

/// Serve one peer: `hello`, then each line from `source` as a request (the
/// [`Grant`] decides what it may do), the replies and the frames it asked for
/// to `sink`. Returns when the peer leaves, the conversation ends, or a
/// `close` request was served.
///
/// The conversation never waits on the peer: a peer that does not read falls
/// behind and gets a `gap` frame, as any subscriber does. Nor does the
/// connection outlive the conversation for long: once it ended, what still
/// waits on a peer that does not read (a reply, the last frames) gets about
/// a second each, then is dropped. Nor can one peer make unbounded work:
/// once the session holds 32 of its requests (a turn's end, a history asked
/// during a turn), one more that would wait is refused with its id (send it
/// again once one is answered), while one answered at once is still served:
/// an `interrupt`, a `close`, an approval answer, a `snapshot`, an `events`
/// (the stream's ring answers it). A burst
/// written in one go meets the cap as requests sent one at a time do: each
/// line is read once the request before it has reached the session. With
/// 64 in flight, the connection reads no more lines until one is answered.
///
/// Dropped before it returns (a transport detached, a host's own `select!`),
/// the connection stops: nothing more is written to `sink`, which is dropped
/// with it (a line being written may be cut short), and no subscription it
/// made goes on.
///
/// # Panics
///
/// When `sink`, `source` or `backend` panics, after the connection has
/// stopped: a host's own bug reaches the host, never reads as the peer
/// leaving. A request whose backend call panicked (an `events` request's
/// `subscribe` among them) is refused with its id first, so its peer is not
/// left waiting for the reply, and the connection stops with it.
///
/// `backend` is any [`Backend`](haest_session::Backend) (a `session.clone()`),
/// an `Arc` of one, or a [`BackendRef`].
pub async fn serve(
    backend: impl Into<BackendRef>,
    grant: Grant,
    start: Start,
    source: impl LineSource,
    sink: impl LineSink,
) -> Ended {
    let backend: BackendRef = backend.into();
    // The peer's lines, each read by a future that owns the source until it
    // resolves. The loop's `select!` makes a `next()` on every pass, polled
    // or not, and drops it whenever another branch comes first; the read
    // inside stays, and `next_line` is called only when the loop polls for a
    // line. The stream never ends: the peer's end is an item (`None`), so
    // the source stays here until the connection ends, as `LineSource`
    // says, and reading stops there.
    let lines = futures::stream::unfold(source, |mut source| async move {
        let line = source.next_line().await;
        Some((line, source))
    });
    let mut lines = std::pin::pin!(lines);
    let (tx, rx) = mpsc::channel(OUT_BUFFER);
    // The writer runs inside this future, not on a task of its own: dropped
    // with it, it calls the sink no more, and the sink goes at once. (An
    // aborted task stops only once it is next polled, and one being polled
    // on another worker goes on writing until that poll ends.)
    let writer = caught(write(sink, rx, backend.meta().sid));
    let mut writer = std::pin::pin!(writer);
    // What the writer ended with, once it was awaited (never awaited twice):
    // Err is the sink's panic.
    let mut writer_done: Option<Result<(), Box<dyn Any + Send>>> = None;
    // The conversation's end, whatever the peer asked for.
    let closed = until_closed(backend.subscribe(u64::MAX));
    tokio::pin!(closed);
    let mut link = Link {
        backend,
        grant,
        tx,
        pump: None,
        follows: 0,
        written: watch::Sender::new(Written::NOTHING),
        tasks: JoinSet::new(),
        placing: None,
        held: Arc::default(),
        panicked: None,
    };
    // Why the connection stopped, or the request whose backend call
    // panicked. No hello: the conversation, or the peer, is already gone.
    let stopped: Result<Ended, Panicked> = if let Err(why) = link.hello(start.token).await {
        Ok(why)
    } else {
        if let Some(after) = start.follow_from {
            link.events(None, after, true);
        }
        // Once the peer's input ends, or the conversation does, what the
        // peer asked for still finishes (a turn it sent runs to its end,
        // a `close` gets its reply) before the connection ends. Once the
        // conversation ended, that is bounded: what still waits on the
        // peer (a reply behind frames a peer that stopped reading never
        // takes) gets the flush grace, no more.
        let mut reading = true;
        let mut why: Option<Ended> = None;
        let writer_done = &mut writer_done;
        let grace = tokio::time::sleep(FLUSH_GRACE);
        tokio::pin!(grace);
        loop {
            if let Some(panicked) = link.panicked.take() {
                break Err(panicked);
            }
            if let Some(why) = why.take_if(|_| link.tasks.is_empty()) {
                break Ok(why);
            }
            let open = !matches!(why, Some(Ended::Closed));
            // At twice the cap, the next line waits until a reply goes out.
            let room = link.tasks.len() < 2 * MAX_IN_FLIGHT;
            // The call read last takes its place first: the next line is
            // read once it has, so the cap sees every request before it.
            let placing = link.placing.is_some();
            tokio::select! {
                () = placed_first(&mut link.placing), if placing => link.placing = None,
                line = lines.next(), if reading && room && !placing => match line.flatten() {
                    None => {
                        reading = false;
                        why.get_or_insert(Ended::PeerLeft);
                    }
                    Some(Err(e)) => break Ok(Ended::Failed(e)),
                    Some(Ok(line)) => link.request(&line),
                },
                joined = link.tasks.join_next(), if !link.tasks.is_empty() => {
                    if let Some(Ok(Some(panicked))) = joined {
                        break Err(panicked);
                    }
                }
                () = &mut closed, if open => {
                    reading = false;
                    why = Some(Ended::Closed);
                    grace.as_mut().reset(tokio::time::Instant::now() + FLUSH_GRACE);
                }
                () = &mut grace, if !open => break Ok(Ended::Closed),
                done = &mut writer => {
                    *writer_done = Some(done);
                    break Ok(Ended::PeerLeft);
                }
            }
        }
    };
    // Its request is still answered, as far as the peer takes it; the
    // writer goes on writing meanwhile.
    let owed = stopped.as_ref().err().and_then(|p| p.id.clone());
    let conversation_ended = matches!(stopped, Ok(Ended::Closed));
    let finishing = async move {
        if let Some(id) = owed {
            let refused = Out::Conn(Outbound::refused(id, BACKEND_PANICKED));
            let _ = tokio::time::timeout(FLUSH_GRACE, link.tx.send(refused)).await;
        }
        link.finish(conversation_ended).await
    };
    let mut finishing = std::pin::pin!(finishing);
    let dropped = loop {
        tokio::select! {
            dropped = &mut finishing => break dropped,
            done = &mut writer, if writer_done.is_none() => writer_done = Some(done),
        }
    };
    if writer_done.is_none() {
        writer_done = tokio::time::timeout(FLUSH_GRACE, &mut writer).await.ok();
    }
    // The host's backend or sink panicked: its bug, never the peer leaving.
    let ended = match (stopped, dropped) {
        (Err(panicked), _) | (Ok(_), Some(panicked)) => panic::resume_unwind(panicked.panic),
        (Ok(ended), None) => ended,
    };
    if let Some(Err(panic)) = writer_done {
        panic::resume_unwind(panic);
    }
    ended
}

/// The refusal a request gets when the backend panicked serving it.
const BACKEND_PANICKED: &str = "the backend failed while it served this request";

/// A backend call that panicked: the id of its request, still owed a reply
/// (none for the stream a connection follows from its start), and the
/// panic, which goes on to the host once the connection has stopped.
struct Panicked {
    id: Option<String>,
    panic: Box<dyn Any + Send>,
}

/// A task a connection started (the pump, a request), aborted when this
/// drops: however `serve` ends, returned or dropped where it waits, nothing
/// it started goes on following the stream. An abort lands at the task's
/// next poll, so one being polled at that moment finishes that step; what
/// it queues goes nowhere, since the writer went with `serve`.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// One connection's state while it is served.
struct Link {
    backend: BackendRef,
    grant: Grant,
    tx: mpsc::Sender<Out>,
    /// The frames it asked for (`events`): one subscription at a time.
    pump: Option<AbortOnDrop<()>>,
    /// Subscriptions made so far: the current one's number.
    follows: u64,
    /// How far the current subscription has been written.
    written: watch::Sender<Written>,
    /// Requests whose reply waits (a turn's end, a change between turns);
    /// one ends with what it owes when its backend call panicked.
    tasks: JoinSet<Option<Panicked>>,
    /// The backend call read last, until it has been polled the first time:
    /// no line is read meanwhile, so the calls reach the backend in the
    /// order they were read, and `held` counts every one read before.
    placing: Option<oneshot::Receiver<()>>,
    /// Backend calls started and not answered yet: what the session holds
    /// for this peer, against [`MAX_IN_FLIGHT`].
    held: Arc<AtomicUsize>,
    /// A panic in the backend on `serve`'s own task (`subscribe` for an
    /// `events`): the connection stops with it before it reads another
    /// line, as with a backend call's.
    panicked: Option<Panicked>,
}

impl Link {
    /// The first frame: who this is, and what the peer may do. Err says why
    /// there is none: the conversation has already ended
    /// ([`Ended::Closed`]), or the writer has, so the peer is gone
    /// ([`Ended::PeerLeft`]).
    async fn hello(&mut self, token: Option<Token>) -> Result<(), Ended> {
        let now = self.backend.snapshot().await;
        let now = now.map_err(|_ended| Ended::Closed)?;
        let hello = Outbound::Hello {
            session: now.session,
            model_spec: now.model_spec,
            pid: self.backend.meta().pid,
            access: self.grant.access,
            token,
        };
        let sent = self.tx.send(Out::Conn(hello)).await;
        sent.map_err(|_writer_gone| Ended::PeerLeft)
    }

    /// One line from the peer. Never waits: what it answers goes out from a
    /// task of its own, so the connection still sees the conversation end
    /// while a peer that stopped reading holds the out buffer full.
    fn request(&mut self, line: &str) {
        let inbound = match decode(line) {
            Ok(inbound) => inbound,
            Err(e) => return self.reply(Outbound::refused(id_of(line), report(&e))),
        };
        let id = inbound.id;
        let req = inbound.request;
        if req.needs_command() && self.grant.access == Access::Watch {
            let why = "this connection may only watch: turns, controls and approval answers need command access";
            return self.reply(Outbound::refused(id, why));
        }
        if self.held.load(Ordering::Relaxed) >= MAX_IN_FLIGHT && !answered_at_once(&req) {
            let why = format!(
                "{MAX_IN_FLIGHT} requests already wait for their replies: send it again once one is answered"
            );
            return self.reply(Outbound::refused(id, why));
        }
        match req {
            Request::Events { after, follow } => self.events(Some(id), after, follow),
            Request::Snapshot => self.spawn(id, false, |b| async move {
                let now = b.snapshot().await?;
                Ok(Some(serde_json::to_value(now).unwrap_or(Value::Null)))
            }),
            Request::History => self.spawn(id, false, |b| async move {
                let h = b.history().await?;
                Ok(Some(json!({"seq": h.seq, "messages": h.messages})))
            }),
            Request::Turn { text } => {
                let input = Input::new(text, self.grant.origin);
                self.spawn(id, true, |b| async move {
                    Ok(Some(outcome(&b.turn(input).await?)))
                });
            }
            Request::ApprovalAnswer { ask, answer } => self.spawn(id, false, move |b| async move {
                b.answer(ask, answer).await?;
                Ok(None)
            }),
            Request::Control(asked) => {
                let change = match asked {
                    wire::Control::Close => return self.close(id),
                    wire::Control::Interrupt => Ok(Control::Interrupt),
                    wire::Control::Clear => Ok(Control::Clear),
                    wire::Control::SetModel { model } if model.trim().is_empty() => {
                        Err("set_model needs a model, provider:model".to_string())
                    }
                    wire::Control::SetModel { model } => Ok(Control::SetModelSpec(model)),
                    wire::Control::SetEffort { effort } => {
                        effort_of(effort.as_deref()).map(Control::SetEffort)
                    }
                    // A verb a newer haest-wire reads and this transport does not serve.
                    _ => Err("this end does not serve that verb".to_string()),
                };
                match change {
                    Ok(change) => self.spawn(id, true, |b| async move {
                        b.control(change).await?;
                        Ok(None)
                    }),
                    Err(why) => self.reply(Outbound::refused(id, why)),
                }
            }
            _ => {
                let why = "this end does not serve that request";
                self.reply(Outbound::refused(id, why));
            }
        }
    }

    /// `close`: the conversation ends, and the reply goes out before the
    /// connection ends with it.
    fn close(&mut self, id: String) {
        self.spawn(id, false, |b| async move {
            b.close().await;
            Ok(None)
        });
    }

    /// An answer that is ready, sent from a task like every other reply: it
    /// counts toward the cap, and it never holds up the connection.
    fn reply(&mut self, reply: Outbound) {
        let tx = self.tx.clone();
        self.tasks.spawn(async move {
            let _ = tx.send(Out::Conn(reply)).await;
            None
        });
    }

    /// Serve a request on its own task; its reply goes out when it is done.
    /// With `after_frames` (a turn, a change), the frames it made go out
    /// first, when the peer follows the stream: the reply to a turn comes
    /// after the turn's last frame.
    ///
    /// The calls reach the backend in the order they were read, whichever
    /// task the runtime runs first: the next line is read once this call
    /// has started (a session takes a call's place in the conversation on
    /// its first poll), so a `clear` sent before a `turn` never clears that
    /// turn, an `interrupt` sent after one stops it, and the cap counts
    /// every request read before the next.
    ///
    /// The backend is the host's code: a panic in it ends the task with the
    /// request's id and the panic, for `serve` to refuse the one and raise
    /// the other.
    fn spawn<F, Fut>(&mut self, id: String, after_frames: bool, run: F)
    where
        F: FnOnce(BackendRef) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<Value>, SessionError>> + Send + 'static,
    {
        let tx = self.tx.clone();
        let backend = self.backend.clone();
        let mut written = self.written.subscribe();
        let (start, started) = oneshot::channel();
        self.placing = Some(started);
        let held = self.held.clone();
        let owed = id.clone();
        self.tasks.spawn(async move {
            let reply = caught(async move {
                let reply = match placed(run(backend.clone()), start, &held).await {
                    Ok(result) => Outbound::done(id, result),
                    Err(e) => Outbound::refused(id, report(&e)),
                };
                if after_frames && let Ok(now) = backend.snapshot().await {
                    let _ = written.wait_for(|w| w.seq >= now.last_seq).await;
                }
                reply
            });
            match reply.await {
                Ok(reply) => {
                    let _ = tx.send(Out::Conn(reply)).await;
                    None
                }
                Err(panic) => Some(Panicked {
                    id: Some(owed),
                    panic,
                }),
            }
        });
    }

    /// The frames after `after` (a `gap` first if the ring lost some), then
    /// the reply to `id`, then with `follow` every new one. Replaces what the
    /// peer asked for before. A panic in the backend's `subscribe`, which
    /// runs here on `serve`'s task, is kept for `serve` to stop with.
    fn events(&mut self, id: Option<String>, after: u64, follow: bool) {
        // The one it replaces stops as it drops.
        self.pump = None;
        self.follows += 1;
        let subscribed = panic::catch_unwind(AssertUnwindSafe(|| self.backend.subscribe(after)));
        let sub = match subscribed {
            Ok(sub) => sub,
            Err(panic) => {
                self.panicked = Some(Panicked { id, panic });
                return;
            }
        };
        let mark = Mark::new(self.written.clone(), self.follows, sub.last_seq());
        // A request in flight like any other, until the pump has replied:
        // input that ends now still gets the catch-up and its reply, and a
        // pump replaced (or stopped) before it replied leaves the reply here.
        let reply = id.map(|id| {
            let (sent, out) = oneshot::channel();
            let (tx, owed) = (self.tx.clone(), id.clone());
            self.tasks.spawn(async move {
                if out.await.is_err() {
                    let why = "its catch-up was not written: a later events request replaced it";
                    let _ = tx.send(Out::Conn(Outbound::refused(owed, why))).await;
                }
                None
            });
            (id, sent)
        });
        let task = pump(sub, self.tx.clone(), reply, follow, mark);
        self.pump = Some(AbortOnDrop(tokio::spawn(task)));
    }

    /// The connection is over: what is queued still goes out, nothing more
    /// does (requests still waiting are dropped; their input stays queued).
    /// When the conversation ended, the stream the peer follows is written to
    /// its last frame first. A request whose backend call had panicked by
    /// then is returned, so its panic is not lost with the rest.
    async fn finish(mut self, closed: bool) -> Option<Panicked> {
        // The subscription stops as it drops, after its last frame or not.
        if let Some(mut pump) = self.pump.take()
            && closed
        {
            let _ = tokio::time::timeout(FLUSH_GRACE, &mut pump).await;
        }
        self.tasks.abort_all();
        let mut panicked = None;
        while let Some(joined) = self.tasks.join_next().await {
            if let Ok(Some(late)) = joined {
                panicked.get_or_insert(late);
            }
        }
        panicked
    }
}

/// How far a connection's subscription has been written: the seq of the last
/// stream frame it sent, [`Written::NOTHING`] while it follows nothing (so
/// no reply waits for frames that will not come).
#[derive(Debug, Clone, Copy)]
struct Written {
    /// Which subscription (a new `events` replaces the last one).
    follow: u64,
    seq: u64,
}

impl Written {
    const NOTHING: Self = Self {
        follow: 0,
        seq: u64::MAX,
    };
}

/// One subscription's marks in [`Written`]; when it ends (or is replaced and
/// aborted) it marks nothing followed, unless a newer one has taken over.
struct Mark {
    written: watch::Sender<Written>,
    follow: u64,
}

impl Mark {
    fn new(written: watch::Sender<Written>, follow: u64, seq: u64) -> Self {
        written.send_replace(Written { follow, seq });
        Self { written, follow }
    }

    fn set(&self, seq: u64) {
        self.written.send_if_modified(|w| {
            let mine = w.follow == self.follow;
            if mine {
                w.seq = seq;
            }
            mine
        });
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        self.set(u64::MAX);
    }
}

/// A subscription's frames to the peer: the catch-up, the reply to `id`
/// (the peer is caught up), then with `follow` everything live until the
/// stream closes, which means the conversation ended. `sent` tells the
/// request's waiter the reply is queued, in the same poll: aborted, the pump
/// either queued it or left it to the waiter, never both.
async fn pump(
    mut sub: Subscription,
    tx: mpsc::Sender<Out>,
    reply: Option<(String, oneshot::Sender<()>)>,
    follow: bool,
    mark: Mark,
) {
    while !sub.caught_up() {
        let Some(frame) = sub.recv().await else { break };
        if tx.send(Out::Stream(frame)).await.is_err() {
            return;
        }
        mark.set(sub.last_seq());
    }
    if let Some((id, sent)) = reply {
        if tx.send(Out::Conn(Outbound::done(id, None))).await.is_err() {
            return;
        }
        let _ = sent.send(());
    }
    if !follow {
        return;
    }
    while let Some(frame) = sub.recv().await {
        if tx.send(Out::Stream(frame)).await.is_err() {
            return;
        }
        mark.set(sub.last_seq());
    }
}

/// What `fut` gives, or the panic that unwound out of it. It is never polled
/// again after a panic (it is dropped), so nothing sees what the panic left
/// half done; the caller ends the connection and raises the panic again.
async fn caught<F: Future>(fut: F) -> Result<F::Output, Box<dyn Any + Send>> {
    let mut fut = std::pin::pin!(fut);
    std::future::poll_fn(|cx| {
        match panic::catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(polled) => polled.map(Ok),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    })
    .await
}

/// A backend call in its place: `started` is told once it has been polled
/// the first time (a [`Backend`](haest_session::Backend) call takes its
/// place in the conversation then). One that did not answer then counts in
/// `held` until it is answered, from just before `started` is told, so
/// whoever waits on `started` sees it counted.
async fn placed<F: Future>(call: F, started: oneshot::Sender<()>, held: &AtomicUsize) -> F::Output {
    let mut call = std::pin::pin!(call);
    let first = std::future::poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx))).await;
    match first {
        Poll::Ready(answer) => {
            let _ = started.send(());
            answer
        }
        Poll::Pending => {
            let _held = Held::new(held);
            let _ = started.send(());
            call.await
        }
    }
}

/// One backend call the session holds, counted while this lives (dropped
/// with the call when its task is aborted, too).
struct Held<'a>(&'a AtomicUsize);

impl<'a> Held<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Resolves once the call read last was first polled (or dropped unpolled,
/// its task gone); never while there is none.
async fn placed_first(placing: &mut Option<oneshot::Receiver<()>>) {
    match placing {
        Some(started) => {
            let _ = started.await;
        }
        None => std::future::pending().await,
    }
}

/// Resolves when the stream closes: the conversation ended.
pub(crate) async fn until_closed(mut sub: Subscription) {
    while sub.recv().await.is_some() {}
}

/// Every line to the peer, in order; a connection frame carries the seq of
/// the last stream frame written before it, and the sink's keepalive goes
/// between lines, busy or idle. Ends when the sink fails.
async fn write(mut sink: impl LineSink, mut rx: mpsc::Receiver<Out>, sid: String) {
    let mut last = 0;
    // After a keepalive that never waited: lines only, until then.
    let mut rest: Option<tokio::time::Instant> = None;
    loop {
        let out = if let Some(until) = rest {
            tokio::select! {
                out = rx.recv() => out,
                () = tokio::time::sleep_until(until) => {
                    rest = None;
                    continue;
                }
            }
        } else {
            let mut keepalive = std::pin::pin!(sink.keepalive());
            let mut waited = false;
            let kept = std::future::poll_fn(|cx| {
                let polled = keepalive.as_mut().poll(cx);
                waited |= polled.is_pending();
                polled.map(|kept| (kept, waited))
            });
            tokio::select! {
                // First, so a stream that is never idle still has its keepalive.
                biased;
                kept = kept => match kept {
                    (Ok(()), true) => continue,
                    (Ok(()), false) => {
                        rest = Some(tokio::time::Instant::now() + KEEPALIVE_REST);
                        continue;
                    }
                    (Err(_), _) => return,
                },
                out = rx.recv() => out,
            }
        };
        let Some(out) = out else { return };
        let line = match out {
            Out::Stream(frame) => {
                if !matches!(frame.body, Body::Wire(Outbound::Gap { .. })) {
                    last = last.max(frame.seq);
                }
                serde_json::to_string(&*frame)
            }
            Out::Conn(body) => {
                let frame = Envelope::connection(sid.as_str(), last, Body::Wire(body));
                serde_json::to_string(&frame)
            }
        };
        let Ok(line) = line else { continue };
        if sink.send_line(line).await.is_err() {
            return;
        }
    }
}

/// A request answered at once, while a turn runs too (an `events` from the
/// stream's ring, the rest by the session): served at the cap
/// ([`MAX_IN_FLIGHT`]), so a peer's stop button, its approval answers and
/// its catch-up never wait behind its own requests.
fn answered_at_once(req: &Request) -> bool {
    matches!(
        req,
        Request::ApprovalAnswer { .. }
            | Request::Snapshot
            | Request::Events { .. }
            | Request::Control(wire::Control::Interrupt | wire::Control::Close)
    )
}

/// A turn's end, as a reply's result: `{"stopReason": ..., "error": ...}`.
fn outcome(end: &TurnOutcome) -> Value {
    json!({"stopReason": end.stop_reason.as_str(), "error": end.error})
}

/// A `set_effort` word as the effort: none (or `default`) is the model's
/// own; Err names the words there are.
fn effort_of(word: Option<&str>) -> Result<Option<Effort>, String> {
    match word {
        None | Some("" | "default") => Ok(None),
        Some(word) => word.parse::<Effort>().map(Some).map_err(|e| e.to_string()),
    }
}

/// The `id` of a line that is not a request this version reads, so its
/// refusal still reaches the one who sent it; empty when it has none.
fn id_of(line: &str) -> String {
    let value: Option<Value> = serde_json::from_str(line).ok();
    let id = value
        .as_ref()
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str);
    id.unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests;
