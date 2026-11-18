//! [`Backend`]: one handle to a live conversation, whatever runs it; and the
//! values that cross it.

use async_trait::async_trait;
use haest_core::{Effort, Message, TurnOutcome};
use haest_wire::{Answer, Origin, Snapshot};

use crate::Subscription;

/// Input for the conversation, and who it counts as. More may come (an
/// image, a file): a host builds one with [`new`](Self::new),
/// [`human`](Self::human) or [`agent`](Self::agent).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Input {
    /// The text.
    pub text: String,
    /// A person or a program: decides how the turn gate counts it.
    pub origin: Origin,
}

impl Input {
    /// `text` from `origin`.
    #[must_use]
    pub fn new(text: impl Into<String>, origin: Origin) -> Self {
        Self {
            text: text.into(),
            origin,
        }
    }

    /// Input a person wrote.
    #[must_use]
    pub fn human(text: impl Into<String>) -> Self {
        Self::new(text, Origin::Human)
    }

    /// Input from another program or agent.
    #[must_use]
    pub fn agent(text: impl Into<String>) -> Self {
        Self::new(text, Origin::Agent)
    }
}

/// A change to the conversation, or a stop. More may come: a backend's
/// `match` refuses what it does not know with a `_` arm.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Control {
    /// Stop the running turn now, or a person's input the rewriter still
    /// holds (its turn ends aborted); nothing between turns.
    Interrupt,
    /// The model from the next turn on, `provider:model`.
    SetModelSpec(String),
    /// The effort from the next turn on; None = the model's own default.
    SetEffort(Option<Effort>),
    /// A fresh session: new id, empty history.
    Clear,
}

/// What a backend says about itself once, for a peer's `hello` and a
/// discovery file. More may come: a backend builds one with
/// [`new`](Self::new).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Meta {
    /// The stream's id.
    pub sid: String,
    /// The process the conversation runs in.
    pub pid: u32,
    /// When the session started, ms since the Unix epoch.
    pub started_ms: u64,
}

impl Meta {
    /// The stream `sid`, run by process `pid`, started at `started_ms` (ms
    /// since the Unix epoch). A transport that names files after the id
    /// (haest-transport's Unix socket) refuses one that is not a plain file
    /// name: keep `/` out of it.
    #[must_use]
    pub fn new(sid: impl Into<String>, pid: u32, started_ms: u64) -> Self {
        Self {
            sid: sid.into(),
            pid,
            started_ms,
        }
    }
}

/// The conversation's messages, and the seq of the last frame they include:
/// follow the stream from that seq to stay current. More may come: a
/// backend builds one with [`new`](Self::new).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct History {
    /// The last frame these messages include.
    pub seq: u64,
    /// The messages, oldest first.
    pub messages: Vec<Message>,
}

impl History {
    /// `messages`, oldest first, current to the frame `seq`.
    #[must_use]
    pub fn new(seq: u64, messages: Vec<Message>) -> Self {
        Self { seq, messages }
    }
}

/// Why a session refused or could not do what it was asked.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The conversation has ended: nothing more runs.
    #[error("the conversation has ended")]
    Closed,
    /// The turn gate held it: too many turns in a row with no message from a
    /// person. It rides in with the next one.
    #[error("held until a person writes: too many turns in a row started without one")]
    Held,
    /// No approval ask with that number waits (answered, withdrawn, or never asked).
    #[error("no approval ask {ask} is waiting")]
    NoSuchAsk {
        /// The ask's number.
        ask: u64,
    },
    /// The conversation could not make the change (a model it cannot use, a
    /// session file it cannot create).
    #[error("{what} failed")]
    Control {
        /// What was asked, in a word or two ("set the model").
        what: &'static str,
        /// Why it failed.
        #[source]
        source: haest_core::error::BoxError,
    },
}

/// One handle to a live conversation: watch it, send it turns, change it,
/// stop it. Transports take any `impl Into<`[`BackendRef`]`>`, so they serve
/// a local session ([`SessionHandle`](crate::SessionHandle), handed over as
/// `session.clone()`) and any other backend alike.
///
/// A call takes its place in the conversation when it is first polled, as
/// [`SessionHandle`](crate::SessionHandle)'s do: a transport starts a peer's
/// requests in the order the peer sent them, each once the one before it
/// was first polled, so a backend that queues a call only later may see them
/// out of order.
///
/// [`SessionHandle`](crate::SessionHandle)'s docs drive one.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Who this is, for a peer's `hello`.
    fn meta(&self) -> Meta;

    /// Queue `input`: it joins a running turn at its next step, or runs as a
    /// turn of its own. Returns once it is queued.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] once the conversation ended.
    async fn submit(&self, input: Input) -> Result<(), SessionError>;

    /// [`submit`](Self::submit), then wait for the end of the turn it
    /// entered history in.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`]; [`SessionError::Held`] when the turn gate
    /// held it.
    async fn turn(&self, input: Input) -> Result<TurnOutcome, SessionError>;

    /// Change the conversation. [`Control::Interrupt`] acts at once; the rest
    /// wait for the running turn to end.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`]; [`SessionError::Control`] when the change
    /// failed (nothing changed then).
    async fn control(&self, control: Control) -> Result<(), SessionError>;

    /// A peer's answer to an approval ask.
    ///
    /// # Errors
    ///
    /// [`SessionError::NoSuchAsk`].
    async fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError>;

    /// The stream's frames after `after`, then every new one (see [`Subscription`]).
    fn subscribe(&self, after: u64) -> Subscription;

    /// The conversation now. A transport asks it first for every peer's
    /// `hello`, and a peer may give up on one that is slow: answer at once,
    /// while a turn or other work runs too.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`].
    async fn snapshot(&self) -> Result<Snapshot, SessionError>;

    /// The messages and the seq they are current to. Asked during a turn, it
    /// answers when that turn ends (the running turn's frames are in the ring).
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`].
    async fn history(&self) -> Result<History, SessionError>;

    /// End the conversation: the running turn is stopped, then everything it
    /// started is stopped and its session saved. Returns once that is done.
    async fn close(&self);
}

haest_core::plug_ref! {
    /// A [`Backend`], shared: what a transport holds while it serves one,
    /// a clone per connection. Built from the backend itself (a
    /// [`SessionHandle`](crate::SessionHandle) clone), or from an `Arc` of
    /// one the host keeps too. Its `Debug` shows the stream's id.
    pub struct BackendRef(dyn Backend);
    debug(backend, f) { f.debug_tuple("BackendRef").field(&backend.meta().sid).finish() }
}
