//! One connection, whatever carries it ([`serve`]): `hello` first, then the
//! peer's requests served against the session, the stream's frames as it
//! asked for them, and a `reply` for every request. Every transport runs its
//! connections through here, so the rules are the same on each.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

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

/// Requests one connection may have waiting for their replies (a turn's
/// end, a history asked mid-turn). At the cap the connection reads no more
/// lines until one is answered, so a peer that only sends makes no more
/// work than this, and the rest wait in its pipe.
const MAX_IN_FLIGHT: usize = 32;

/// Lines a peer sends: one request each.
pub trait LineSource: Send {
    /// The next line; None when the peer is gone.
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
/// once 32 of its requests wait for their replies, the connection reads no
/// more lines until one is answered.
///
/// Dropped before it returns (a transport detached, a host's own `select!`),
/// the connection stops: nothing more is written to `sink`, which is dropped
/// with it, and no subscription it made goes on.
///
/// `backend` is any [`Backend`](haest_session::Backend) (a `session.clone()`),
/// an `Arc` of one, or a [`BackendRef`].
pub async fn serve(
    backend: impl Into<BackendRef>,
    grant: Grant,
    start: Start,
    mut source: impl LineSource,
    sink: impl LineSink,
) -> Ended {
    let backend: BackendRef = backend.into();
    let (tx, rx) = mpsc::channel(OUT_BUFFER);
    let mut writer = AbortOnDrop(tokio::spawn(write(sink, rx, backend.meta().sid)));
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
    };
    // No hello: the conversation, or the peer, is already gone.
    let ended = if let Err(why) = link.hello(start.token).await {
        why
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
        let grace = tokio::time::sleep(FLUSH_GRACE);
        tokio::pin!(grace);
        loop {
            if let Some(why) = why.take_if(|_| link.tasks.is_empty()) {
                break why;
            }
            let open = !matches!(why, Some(Ended::Closed));
            // At the cap, the next line waits until a reply goes out.
            let room = link.tasks.len() < MAX_IN_FLIGHT;
            tokio::select! {
                line = source.next_line(), if reading && room => match line {
                    None => {
                        reading = false;
                        why.get_or_insert(Ended::PeerLeft);
                    }
                    Some(Err(e)) => break Ended::Failed(e),
                    Some(Ok(line)) => link.request(&line),
                },
                _ = link.tasks.join_next(), if !link.tasks.is_empty() => {}
                () = &mut closed, if open => {
                    reading = false;
                    why = Some(Ended::Closed);
                    grace.as_mut().reset(tokio::time::Instant::now() + FLUSH_GRACE);
                }
                () = &mut grace, if !open => break Ended::Closed,
                _ = &mut writer => break Ended::PeerLeft,
            }
        }
    };
    link.finish(matches!(ended, Ended::Closed)).await;
    if !writer.0.is_finished() {
        let _ = tokio::time::timeout(FLUSH_GRACE, &mut writer).await;
    }
    ended
}

/// A task a connection started, aborted when this drops: however `serve`
/// ends, returned or dropped where it waits, nothing it started goes on
/// writing to the peer or following the stream.
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
    /// Requests whose reply waits (a turn's end, a change between turns).
    tasks: JoinSet<()>,
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
                let input = Input {
                    text,
                    origin: self.grant.origin,
                };
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
        let (backend, tx) = (self.backend.clone(), self.tx.clone());
        self.tasks.spawn(async move {
            backend.close().await;
            let _ = tx.send(Out::Conn(Outbound::done(id, None))).await;
        });
    }

    /// An answer that is ready, sent from a task like every other reply: it
    /// counts toward the cap, and it never holds up the connection.
    fn reply(&mut self, reply: Outbound) {
        let tx = self.tx.clone();
        self.tasks.spawn(async move {
            let _ = tx.send(Out::Conn(reply)).await;
        });
    }

    /// Serve a request on its own task; its reply goes out when it is done.
    /// With `after_frames` (a turn, a change), the frames it made go out
    /// first, when the peer follows the stream: the reply to a turn comes
    /// after the turn's last frame.
    fn spawn<F, Fut>(&mut self, id: String, after_frames: bool, run: F)
    where
        F: FnOnce(BackendRef) -> Fut,
        Fut: Future<Output = Result<Option<Value>, SessionError>> + Send + 'static,
    {
        let tx = self.tx.clone();
        let backend = self.backend.clone();
        let mut written = self.written.subscribe();
        let done = run(self.backend.clone());
        self.tasks.spawn(async move {
            let reply = match done.await {
                Ok(result) => Outbound::done(id, result),
                Err(e) => Outbound::refused(id, report(&e)),
            };
            if after_frames && let Ok(now) = backend.snapshot().await {
                let _ = written.wait_for(|w| w.seq >= now.last_seq).await;
            }
            let _ = tx.send(Out::Conn(reply)).await;
        });
    }

    /// The frames after `after` (a `gap` first if the ring lost some), then
    /// the reply to `id`, then with `follow` every new one. Replaces what the
    /// peer asked for before.
    fn events(&mut self, id: Option<String>, after: u64, follow: bool) {
        // The one it replaces stops as it drops.
        self.pump = None;
        self.follows += 1;
        let sub = self.backend.subscribe(after);
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
            });
            (id, sent)
        });
        let task = pump(sub, self.tx.clone(), reply, follow, mark);
        self.pump = Some(AbortOnDrop(tokio::spawn(task)));
    }

    /// The connection is over: what is queued still goes out, nothing more
    /// does (requests still waiting are dropped; their input stays queued).
    /// When the conversation ended, the stream the peer follows is written to
    /// its last frame first.
    async fn finish(mut self, closed: bool) {
        // The subscription stops as it drops, after its last frame or not.
        if let Some(mut pump) = self.pump.take()
            && closed
        {
            let _ = tokio::time::timeout(FLUSH_GRACE, &mut pump).await;
        }
        self.tasks.shutdown().await;
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
