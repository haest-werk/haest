//! [`Hub`]: one stream's frames, numbered, kept in a ring and handed to every
//! subscriber; and the approval asks waiting on a peer.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use haest_core::LoopEvent;
use haest_wire::{Answer, Answerer, Framer, Level, Outbound, Path, Ring};
use tokio::sync::{broadcast, oneshot};

use crate::{Body, Frame, SessionError, Subscription};

/// How a [`Hub`] keeps frames ([`Hub::new_with_options`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubOptions {
    /// Frames the ring keeps for catch-up besides the running turn (the
    /// `event_ring` setting); at least 1. Default 1000.
    pub ring: usize,
    /// Frames a subscriber may fall behind before it gets a `gap`; at least
    /// 1. Default 4096.
    pub buffer: usize,
}

impl Default for HubOptions {
    fn default() -> Self {
        Self {
            ring: 1000,
            buffer: 4096,
        }
    }
}

/// One stream: every frame published here gets the next seq, goes into the
/// ring and out to every subscriber, in one order for all.
///
/// [`publish`](Self::publish) never waits: a subscriber that falls behind
/// gets one `gap` frame for what it missed, and the turn goes on. Cheap to
/// clone; every clone is the same stream. A host makes one before it builds
/// the conversation, so what the build says (a notice) is already numbered,
/// then starts the session over it ([`start`](crate::start)).
///
/// ```
/// use haest_session::Hub;
/// use haest_wire::Level;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let hub = Hub::new("s1");
/// let mut early = hub.subscribe(0);
/// hub.notice(Level::Info, "building");
/// hub.notice(Level::Warning, "an MCP server did not start");
///
/// let first = early.recv().await;
/// assert_eq!(first.map(|f| (f.seq, f.sid.clone())), Some((1, "s1".to_string())));
///
/// // A late subscriber catches up from the ring.
/// let mut late = hub.subscribe(1);
/// assert_eq!(late.recv().await.map(|f| f.seq), Some(2));
/// # }
/// ```
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

struct Inner {
    sid: String,
    state: Mutex<State>,
}

struct State {
    framer: Framer,
    ring: Ring<Body>,
    /// None once closed: subscriptions end after what they hold.
    tx: Option<broadcast::Sender<Arc<Frame>>>,
    asks: Asks,
}

#[derive(Default)]
struct Asks {
    #[cfg(feature = "approvals")]
    next: u64,
    waiting: HashMap<u64, oneshot::Sender<Answer>>,
}

/// An approval ask as the hub publishes it.
#[cfg(feature = "approvals")]
pub(crate) struct AskInfo {
    pub tool_name: String,
    pub tool_call_id: Option<String>,
    pub description: String,
    pub preview: String,
    pub key: Option<String>,
}

impl Hub {
    /// A hub for the stream `sid` (one per running conversation), with
    /// nothing published yet and the default [`HubOptions`]. A transport
    /// that names files after the id (haest-transport's Unix socket)
    /// refuses one that is not a plain file name: keep `/` out of it.
    pub fn new(sid: impl Into<String>) -> Self {
        Self::new_with_options(sid, HubOptions::default())
    }

    /// A hub for the stream `sid` that keeps frames as `opts` says. They are
    /// fixed for the hub's life, the same for every clone and subscriber.
    ///
    /// ```
    /// use haest_session::{Hub, HubOptions};
    /// use haest_wire::Level;
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let opts = HubOptions { ring: 2, ..HubOptions::default() };
    /// let hub = Hub::new_with_options("s1", opts);
    /// for n in 1..=3 {
    ///     hub.notice(Level::Info, format!("line {n}"));
    /// }
    /// // The ring kept the last two: a peer from the start gets a gap first.
    /// let mut late = hub.subscribe(0);
    /// let first = late.recv().await.map(|f| f.body.clone());
    /// assert!(matches!(first, Some(haest_session::Body::Wire(haest_wire::Outbound::Gap { .. }))));
    /// # }
    /// ```
    pub fn new_with_options(sid: impl Into<String>, opts: HubOptions) -> Self {
        let sid = sid.into();
        let (tx, _) = broadcast::channel(opts.buffer.max(1));
        let state = State {
            framer: Framer::new(sid.clone()),
            ring: Ring::new(opts.ring),
            tx: Some(tx),
            asks: Asks::default(),
        };
        Self {
            inner: Arc::new(Inner {
                sid,
                state: Mutex::new(state),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().expect("the hub's lock is poisoned")
    }

    /// The stream's id.
    pub fn sid(&self) -> &str {
        &self.inner.sid
    }

    /// The seq of the last frame published; 0 before the first.
    pub fn last_seq(&self) -> u64 {
        self.lock().framer.seq()
    }

    /// `body`, about `path`, as the stream's next frame: numbered, kept, and
    /// sent to every subscriber. Returns its seq. After [`close`](Self::close)
    /// it is still numbered and kept, but nobody gets it.
    pub fn publish(&self, path: Path, body: impl Into<Body>) -> u64 {
        // A host's own `From<_> for Body` runs here, never under the lock.
        let body = body.into();
        let mut state = self.lock();
        let frame = Arc::new(state.framer.stamp(path, body));
        let seq = frame.seq;
        state.ring.push(Arc::clone(&frame));
        if let Some(tx) = &state.tx {
            // No subscriber is not an error: the ring has it.
            let _ = tx.send(frame);
        }
        seq
    }

    /// One of the conversation's own events, about `["main"]`.
    pub fn event(&self, ev: LoopEvent) -> u64 {
        self.publish(Path::main(), ev)
    }

    /// A line for the human.
    pub fn notice(&self, level: Level, text: impl Into<String>) -> u64 {
        let text = text.into();
        self.publish(Path::main(), Outbound::Notice { level, text })
    }

    /// The frames after `after` from the ring (a `gap` first when it lost
    /// some), then every new one. Nothing is published between the two.
    ///
    /// An `after` past the last seq published is not this stream's (a peer
    /// that holds another run's frames under the same id): the subscription
    /// starts at the last seq with a `gap`, so the peer catches up
    /// (`history`) and no new frame is skipped as one it holds.
    pub fn subscribe(&self, after: u64) -> Subscription {
        let state = self.lock();
        let last = state.framer.seq();
        let after_end = after > last;
        let after = after.min(last);
        let mut replay = state.ring.since(after);
        replay.gap |= after_end;
        let rx = match &state.tx {
            Some(tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        };
        Subscription::new(self.inner.sid.clone(), after, replay, rx)
    }

    /// No more frames go out: every subscription ends once it has what it
    /// holds. Waiting approval asks go unanswered (their prompts deny).
    /// Never panics, so it may run while a panic unwinds.
    pub fn close(&self) {
        // A poisoned lock still holds what there is to close.
        let state = self.inner.state.lock();
        let mut state = state.unwrap_or_else(PoisonError::into_inner);
        state.tx = None;
        state.asks.waiting.clear();
    }

    /// Publish an approval ask; the receiver gets the answer. After
    /// [`close`](Self::close) nobody can answer: the receiver is closed at
    /// once and nothing is published.
    #[cfg(feature = "approvals")]
    pub(crate) fn ask(&self, info: AskInfo) -> (u64, oneshot::Receiver<Answer>) {
        let (tx, rx) = oneshot::channel();
        let ask = {
            let mut state = self.lock();
            state.asks.next += 1;
            let ask = state.asks.next;
            if state.tx.is_none() {
                // `tx` drops here: its prompt reads a closed hub, a deny.
                return (ask, rx);
            }
            state.asks.waiting.insert(ask, tx);
            ask
        };
        let frame = Outbound::ApprovalAsk {
            ask,
            tool_name: info.tool_name,
            tool_call_id: info.tool_call_id,
            description: info.description,
            preview: info.preview,
            key: info.key,
        };
        self.publish(Path::main(), frame);
        (ask, rx)
    }

    /// The ask is given up on (an interrupt, the timeout): an answer now goes
    /// nowhere, and the stream says so. Nothing when it was answered.
    #[cfg(feature = "approvals")]
    pub(crate) fn withdraw(&self, ask: u64) {
        let waiting = self.lock().asks.waiting.remove(&ask);
        if waiting.is_some() {
            self.publish(Path::main(), Outbound::ApprovalWithdrawn { ask });
        }
    }

    /// The ask was answered on the conversation's own side (`by`, the
    /// terminal), not by a peer: a peer's answer now is refused, and the
    /// stream says who answered. False, and nothing published, when it no
    /// longer waits (a peer answered first, or it was withdrawn).
    #[cfg(feature = "approvals")]
    pub(crate) fn answered(&self, ask: u64, answer: Answer, by: Answerer) -> bool {
        let waiting = self.lock().asks.waiting.remove(&ask);
        if waiting.is_some() {
            self.publish(Path::main(), Outbound::ApprovalAnswered { ask, answer, by });
        }
        waiting.is_some()
    }

    /// A peer's answer to the ask `ask`. The first answer wins: the stream
    /// says it was a peer's (`approval_answered`), and a later one, from a
    /// peer or the terminal, is refused.
    ///
    /// # Errors
    ///
    /// [`SessionError::NoSuchAsk`] when no such ask waits (answered,
    /// withdrawn, or never asked).
    pub fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError> {
        let waiting = self.lock().asks.waiting.remove(&ask);
        let Some(prompt) = waiting else {
            return Err(SessionError::NoSuchAsk { ask });
        };
        if prompt.send(answer).is_err() {
            // Its prompt was given up on and is about to withdraw it; it is
            // no longer listed, so the stream is told here.
            self.publish(Path::main(), Outbound::ApprovalWithdrawn { ask });
            return Err(SessionError::NoSuchAsk { ask });
        }
        let by = Answerer::Peer;
        self.publish(Path::main(), Outbound::ApprovalAnswered { ask, answer, by });
        Ok(())
    }
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("sid", &self.inner.sid)
            .field("last_seq", &self.last_seq())
            .finish()
    }
}
