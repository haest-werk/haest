//! A member's life as its parent sees it: the start that may refuse or run it
//! elsewhere ([`SubagentParent::start_member`](super::SubagentParent::start_member)),
//! the run it hands back ([`Member`], over a [`MemberRun`]), and how it ended
//! ([`MemberEnd`]).

use std::{fmt, time::Duration};

use async_trait::async_trait;
use haest_core::{SharedDeps, caps::SubagentCaps, messages::Usage};

use super::SpawnInput;

/// One member's own caps: its turn's spend, model calls and wall clock. 0 (or
/// a zero duration) is no cap, as in [`TurnCaps`](haest_core::caps::TurnCaps).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemberCaps {
    /// Its turn's spend in USD.
    pub usd: f64,
    /// Its turn's model calls.
    pub steps: u32,
    /// Its turn's wall clock.
    pub time: Duration,
}

impl From<SubagentCaps> for MemberCaps {
    /// The per-agent caps of the tree's caps.
    fn from(c: SubagentCaps) -> Self {
        Self {
            usd: c.usd,
            steps: c.steps,
            time: c.time,
        }
    }
}

/// What the hub knows of a member about to start: who it is, what it was
/// asked, and how it would run if the parent changes nothing.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MemberStart {
    /// Its address, unique in the hub.
    pub name: String,
    /// Its id, unique across hubs: the key of every [`SubagentEvent`](super::SubagentEvent).
    pub id: String,
    /// The agent that started it.
    pub parent: String,
    /// How deep it runs (a subagent of main is 1).
    pub depth: u32,
    /// The Agent call: description, prompt, model spec, kind, background, budget.
    pub input: SpawnInput,
    /// The model spec it runs on unless its run names another: the call's,
    /// else the parent's default.
    pub model_spec: String,
    /// The caps it runs under unless its run names others: the hub's, or the
    /// call's budget within them.
    pub caps: MemberCaps,
}

/// How a member's turn ended.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Ending {
    /// It ended its turn with an answer.
    Answered,
    /// StopAgent (or the hub's `stop_all`) stopped it.
    Stopped,
    /// The turn that started it, a foreground member's caller, was cancelled.
    CallerCancelled,
    /// A cap stopped its turn.
    Capped(
        /// Which cap, as the loop said it.
        String,
    ),
    /// Its turn failed (a model error, a refusal) or ended with no answer.
    Failed(
        /// Why, as the loop said it.
        String,
    ),
    /// Its loop panicked.
    Crashed(
        /// The panic's message.
        String,
    ),
}

/// What a member's turn spent, already charged to the ledger by its own loop.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spend {
    /// Its turn's tokens and cost, every model call and tool spend included.
    pub usage: Usage,
    /// The model calls it made.
    pub model_calls: u32,
}

impl Spend {
    /// The USD it spent; 0 when no call reported a price.
    pub fn usd(&self) -> f64 {
        self.usage.cost_usd.unwrap_or(0.0)
    }
}

/// A member's end, handed to its run ([`MemberRun::end`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MemberEnd {
    /// How its turn ended.
    pub ending: Ending,
    /// Its answer as its caller gets it: the final text, any "stopped early"
    /// note, and messages that reached it too late.
    pub answer: String,
    /// What it spent.
    pub spend: Spend,
}

/// A started member's run: what its parent decided about it, and where its
/// end goes. Wrapped in a [`Member`] and returned by
/// [`SubagentParent::start_member`](super::SubagentParent::start_member);
/// every method has a default, so [`Member::in_process`] (all defaults) is
/// the member haest has always run.
///
/// # Examples
///
/// A run kept by the host: it names the model spec its start resolved, tighter
/// caps, and hears the end.
///
/// ```
/// use std::{sync::{Arc, Mutex}, time::Duration};
///
/// use async_trait::async_trait;
/// use haest_tools::{Ending, Member, MemberCaps, MemberEnd, MemberRun};
///
/// struct NodeRun {
///     model_spec: String,
///     closed: Arc<Mutex<Vec<(Ending, f64)>>>,
/// }
///
/// #[async_trait]
/// impl MemberRun for NodeRun {
///     fn model_spec(&self) -> Option<String> {
///         Some(self.model_spec.clone())
///     }
///     fn caps(&self) -> Option<MemberCaps> {
///         Some(MemberCaps { usd: 0.2, steps: 10, time: Duration::from_secs(120) })
///     }
///     async fn end(self: Box<Self>, end: MemberEnd) {
///         // Here the host would close its run: flush its records, then stop it.
///         self.closed.lock().unwrap().push((end.ending, end.spend.usd()));
///     }
/// }
///
/// let closed = Arc::new(Mutex::new(Vec::new()));
/// // What start_member hands back.
/// let member = Member::new(NodeRun { model_spec: "openrouter:a/b".into(), closed });
/// # let _ = member;
/// ```
///
/// [`SubagentParent::start_member`](super::SubagentParent::start_member)
/// has a parent that returns one.
#[async_trait]
pub trait MemberRun: Send + Sync {
    /// The model spec it runs on (a host may resolve a short name); None =
    /// the call's, else the parent's default ([`MemberStart::model_spec`]).
    fn model_spec(&self) -> Option<String> {
        None
    }
    /// Its turn's caps; None = [`MemberStart::caps`]. The subagent note
    /// tells the member these.
    fn caps(&self) -> Option<MemberCaps> {
        None
    }
    /// Its own shared deps (models over its own credentials, hooks that keep
    /// its own records); None = the parent's
    /// [`deps`](super::SubagentParent::deps).
    ///
    /// Its own deps are used as they are, hooks included; with the parent's,
    /// the member persists nothing and its tools' inner calls go to
    /// [`on_subagent`](super::SubagentParent::on_subagent). Either way the
    /// hub sets its tools, its system prompt and its turn caps.
    fn deps(&self) -> Option<SharedDeps> {
        None
    }
    /// Its turn is over and it has left the hub: how it ended, its answer and
    /// its spend. Awaited before the answer goes back to its caller (or to
    /// whoever reads a background answer); called exactly once.
    async fn end(self: Box<Self>, end: MemberEnd) {
        let _ = end;
    }
}

/// The default member: every [`MemberRun`] default.
struct InProcessRun;

impl MemberRun for InProcessRun {}

/// A started member, as
/// [`SubagentParent::start_member`](super::SubagentParent::start_member)
/// hands it back: [`Member::in_process`] (the default), or a host's own
/// [`MemberRun`] ([`Member::new`], or `.into()` from one).
pub struct Member(Box<dyn MemberRun>);

impl Member {
    /// A member run by `run`: its model spec, caps and deps where it names
    /// them, and its end.
    pub fn new(run: impl MemberRun + 'static) -> Self {
        Self(Box::new(run))
    }

    /// The default member: a loop in this process on the parent's deps, the
    /// call's model spec and the hub's caps. What
    /// [`SubagentParent::start_member`](super::SubagentParent::start_member)
    /// returns unless a parent says otherwise.
    pub fn in_process() -> Self {
        Self::new(InProcessRun)
    }

    pub(super) fn run(&self) -> &dyn MemberRun {
        &*self.0
    }

    pub(super) fn into_run(self) -> Box<dyn MemberRun> {
        self.0
    }
}

impl Default for Member {
    fn default() -> Self {
        Self::in_process()
    }
}

impl<T: MemberRun + 'static> From<T> for Member {
    fn from(run: T) -> Self {
        Self::new(run)
    }
}

impl fmt::Debug for Member {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Member")
            .field("model_spec", &self.0.model_spec())
            .field("caps", &self.0.caps())
            .finish_non_exhaustive()
    }
}
