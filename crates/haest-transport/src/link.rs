//! One connection, whatever carries it ([`serve`]): `hello` first, then the
//! peer's requests served against the session, the stream's frames as it
//! asked for them, and a `reply` for every request. Every transport runs its
//! connections through here, so the rules are the same on each.

use std::{future::Future, sync::Arc, time::Duration};

use haest_core::{Effort, TurnOutcome, error::report};
use haest_session::{BackendRef, Body, Control, Frame, Input, SessionError, Subscription};
use haest_wire::{self as wire, Access, Envelope, Outbound, Request, Token, decode};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};

use crate::{Grant, TransportError};

/// Frames waiting for a slow peer before its subscription falls behind (and
/// gets a `gap`): the conversation never waits on a peer.
const OUT_BUFFER: usize = 1024;

/// How long a connection that ends gets to write what it still holds.
const FLUSH_GRACE: Duration = Duration::from_secs(1);

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
    /// Reading from the peer failed.
    Failed(TransportError),
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
/// behind and gets a `gap` frame, as any subscriber does.
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
    let mut writer = tokio::spawn(write(sink, rx, backend.meta().sid));
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
    let ended = match link.hello(start.token).await {
        Err(()) => Ended::Closed,
        Ok(()) => {
            if let Some(after) = start.follow_from {
                link.events(None, after, true);
            }
            // Once the peer's input ends, or the conversation does, what the
            // peer asked for still finishes (a turn it sent runs to its end,
            // a `close` gets its reply) before the connection ends.
            let mut reading = true;
            let mut why: Option<Ended> = None;
            loop {
                if let Some(why) = why.take_if(|_| link.tasks.is_empty()) {
                    break why;
                }
                let open = !matches!(why, Some(Ended::Closed));
                tokio::select! {
                    line = source.next_line(), if reading => match line {
                        None => {
                            reading = false;
                            why.get_or_insert(Ended::PeerLeft);
                        }
                        Some(Err(e)) => break Ended::Failed(e),
                        Some(Ok(line)) => link.request(&line).await,
                    },
                    _ = link.tasks.join_next(), if !link.tasks.is_empty() => {}
                    () = &mut closed, if open => {
                        reading = false;
                        why = Some(Ended::Closed);
                    }
                    _ = &mut writer => break Ended::PeerLeft,
                }
            }
        }
    };
    link.finish(matches!(ended, Ended::Closed)).await;
    if !writer.is_finished() {
        let _ = tokio::time::timeout(FLUSH_GRACE, &mut writer).await;
    }
    writer.abort();
    ended
}

/// One connection's state while it is served.
struct Link {
    backend: BackendRef,
    grant: Grant,
    tx: mpsc::Sender<Out>,
    /// The frames it asked for (`events`): one subscription at a time.
    pump: Option<JoinHandle<()>>,
    /// Subscriptions made so far: the current one's number.
    follows: u64,
    /// How far the current subscription has been written.
    written: watch::Sender<Written>,
    /// Requests whose reply waits (a turn's end, a change between turns).
    tasks: JoinSet<()>,
}

impl Link {
    /// The first frame: who this is, and what the peer may do. Err when the
    /// conversation has already ended.
    async fn hello(&mut self, token: Option<Token>) -> Result<(), ()> {
        let now = self.backend.snapshot().await.map_err(|_| ())?;
        let hello = Outbound::Hello {
            session: now.session,
            model_spec: now.model_spec,
            pid: self.backend.meta().pid,
            access: self.grant.access,
            token,
        };
        self.tx.send(Out::Conn(hello)).await.map_err(|_| ())
    }

    /// One line from the peer.
    async fn request(&mut self, line: &str) {
        let inbound = match decode(line) {
            Ok(inbound) => inbound,
            Err(e) => return self.reply(Outbound::refused(id_of(line), report(&e))).await,
        };
        let id = inbound.id;
        let req = inbound.request;
        if req.needs_command() && self.grant.access == Access::Watch {
            let why = "this connection may only watch: turns, controls and approval answers need command access";
            return self.reply(Outbound::refused(id, why)).await;
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
                    Err(why) => self.reply(Outbound::refused(id, why)).await,
                }
            }
            _ => {
                let why = "this end does not serve that request";
                self.reply(Outbound::refused(id, why)).await;
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

    async fn reply(&self, reply: Outbound) {
        let _ = self.tx.send(Out::Conn(reply)).await;
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
        if let Some(old) = self.pump.take() {
            old.abort();
        }
        self.follows += 1;
        let sub = self.backend.subscribe(after);
        let mark = Mark::new(self.written.clone(), self.follows, sub.last_seq());
        let task = pump(sub, self.tx.clone(), id, follow, mark);
        self.pump = Some(tokio::spawn(task));
    }

    /// The connection is over: what is queued still goes out, nothing more
    /// does (requests still waiting are dropped; their input stays queued).
    /// When the conversation ended, the stream the peer follows is written to
    /// its last frame first.
    async fn finish(mut self, closed: bool) {
        if let Some(mut pump) = self.pump.take()
            && (!closed || tokio::time::timeout(FLUSH_GRACE, &mut pump).await.is_err())
        {
            pump.abort();
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
/// stream closes, which means the conversation ended.
async fn pump(
    mut sub: Subscription,
    tx: mpsc::Sender<Out>,
    id: Option<String>,
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
    if let Some(id) = id {
        let _ = tx.send(Out::Conn(Outbound::done(id, None))).await;
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
/// the last stream frame written before it. Ends when the sink fails.
async fn write(mut sink: impl LineSink, mut rx: mpsc::Receiver<Out>, sid: String) {
    let mut last = 0;
    while let Some(out) = rx.recv().await {
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
