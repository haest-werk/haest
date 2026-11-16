//! [`Hub`]: one stream's frames, numbered, kept in a ring and handed to every
//! subscriber; and the approval asks waiting on a peer.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use haest_core::LoopEvent;
use haest_wire::{Answer, Framer, Level, Outbound, Path, Ring};
use tokio::sync::{broadcast, oneshot};

use crate::{Body, Frame, SessionError, Subscription};

/// How a [`Hub`] keeps frames.
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
/// use haest_session::{Hub, HubOptions};
/// use haest_wire::Level;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let hub = Hub::new("s1", HubOptions::default());
/// let mut early = hub.subscribe(0);
/// hub.notice(Level::Info, "building");
/// hub.notice(Level::Warning, "an MCP server did not start");
///
/// let first = early.recv().await.unwrap();
/// assert_eq!((first.seq, first.sid.as_str()), (1, "s1"));
///
/// // A late subscriber catches up from the ring.
/// let mut late = hub.subscribe(1);
/// assert_eq!(late.recv().await.unwrap().seq, 2);
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
    pub description: String,
    pub preview: String,
    pub key: Option<String>,
}

impl Hub {
    /// A hub for the stream `sid` (one per running conversation), with
    /// nothing published yet.
    pub fn new(sid: impl Into<String>, opts: HubOptions) -> Self {
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

    /// The stream's id.
    pub fn sid(&self) -> &str {
        &self.inner.sid
    }

    /// The seq of the last frame published; 0 before the first.
    pub fn last_seq(&self) -> u64 {
        self.inner.state.lock().unwrap().framer.seq()
    }

    /// `body`, about `path`, as the stream's next frame: numbered, kept, and
    /// sent to every subscriber. Returns its seq. After [`close`](Self::close)
    /// it is still numbered and kept, but nobody gets it.
    pub fn publish(&self, path: Path, body: impl Into<Body>) -> u64 {
        let mut state = self.inner.state.lock().unwrap();
        let frame = Arc::new(state.framer.stamp(path, body.into()));
        let seq = frame.seq;
        state.ring.push(frame.clone());
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
    pub fn subscribe(&self, after: u64) -> Subscription {
        let state = self.inner.state.lock().unwrap();
        let replay = state.ring.since(after);
        let rx = match &state.tx {
            Some(tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        };
        Subscription::new(self.inner.sid.clone(), after, replay, rx)
    }

    /// No more frames go out: every subscription ends once it has what it
    /// holds. Waiting approval asks go unanswered (their prompts deny).
    pub fn close(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.tx = None;
        state.asks.waiting.clear();
    }

    /// Publish an approval ask; the receiver gets the answer.
    #[cfg(feature = "approvals")]
    pub(crate) fn ask(&self, info: AskInfo) -> (u64, oneshot::Receiver<Answer>) {
        let (tx, rx) = oneshot::channel();
        let ask = {
            let mut state = self.inner.state.lock().unwrap();
            state.asks.next += 1;
            let ask = state.asks.next;
            state.asks.waiting.insert(ask, tx);
            ask
        };
        let frame = Outbound::ApprovalAsk {
            ask,
            tool_name: info.tool_name,
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
        let waiting = self.inner.state.lock().unwrap().asks.waiting.remove(&ask);
        if waiting.is_some() {
            self.publish(Path::main(), Outbound::ApprovalWithdrawn { ask });
        }
    }

    /// A peer's answer to the ask `ask`.
    ///
    /// # Errors
    ///
    /// [`SessionError::NoSuchAsk`] when no such ask waits (answered,
    /// withdrawn, or never asked).
    pub fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError> {
        let waiting = self.inner.state.lock().unwrap().asks.waiting.remove(&ask);
        match waiting.map(|tx| tx.send(answer)) {
            Some(Ok(())) => Ok(()),
            _ => Err(SessionError::NoSuchAsk { ask }),
        }
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
