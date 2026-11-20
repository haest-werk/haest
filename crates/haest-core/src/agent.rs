//! The agent loop. One [`Agent::run`] = one human turn: model call, run the
//! tools it asked for, feed results back, repeat until the model answers
//! without tools.
//!
//! Start from [`Agent::new`] with [`AgentDeps`] and [`AgentOptions`]. Steer or
//! stop a running turn from elsewhere through [`AgentControl`].
//!
//! Invariants the loop keeps so every provider accepts the history:
//!   - every tool call gets exactly one tool result, even on interrupt (a
//!     turn dropped mid-tool has its calls answered when the next one starts);
//!   - input that arrives mid-turn ([`AgentControl::steer`]) is queued and
//!     injected at the next step boundary (after the tool results), never dropped;
//!   - the turn guard (every cap, and the ledger) is asked before every model
//!     call, and its nudge rides in at the same step boundary.
//!
//! Within one model response, read-only tool calls run in parallel
//! (one batch per run of consecutive read-only calls); their results still
//! land in call order.

mod control;
mod durable;
#[cfg(test)]
pub(crate) mod test_support;
mod turn;

use std::{fmt, time::Instant};

use tokio_util::sync::CancellationToken;

use crate::{
    caps::TurnCaps,
    compaction::last_context_tokens,
    deps::AgentDeps,
    events::{EventSink, Forward, TurnOutcome},
    guard::{SharedWatch, TurnGuard},
    media::fit_for_model,
    messages::{Message, StopReason, Usage},
    provider::Effort,
    request_prefix::{RequestPrefix, build_request_prefix},
    threshold::Watch,
};

pub use control::AgentControl;
use control::Running;
use turn::Turn;

/// How a new [`Agent`] starts: its model, its effort, and any history to resume.
///
/// ```
/// use haest_core::{AgentOptions, Message, provider::Effort};
///
/// let resumed = AgentOptions::new("openrouter:example/model-1")
///     .with_effort(Effort::High)
///     .with_history(vec![Message::user("hi")]);
/// assert_eq!(resumed.effort(), Some(Effort::High));
/// assert_eq!(resumed.history().len(), 1);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct AgentOptions {
    model_spec: String,
    effort: Option<Effort>,
    history: Vec<Message>,
}

impl AgentOptions {
    /// Options for `model_spec` with no effort and no history.
    pub fn new(model_spec: impl Into<String>) -> Self {
        Self {
            model_spec: model_spec.into(),
            effort: None,
            history: Vec::new(),
        }
    }

    /// The same, asking for `effort`. Without it none is sent, and the model
    /// uses its default.
    #[must_use]
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// The same, resuming from `history` (a replayed session's messages). A
    /// tool call in it with no result is answered as interrupted when the
    /// first turn or compaction starts, and a tool result without its call
    /// dropped, as [`replay`](crate::session::replay) repairs them.
    #[must_use]
    pub fn with_history(mut self, history: impl Into<Vec<Message>>) -> Self {
        self.history = history.into();
        self
    }

    /// The model spec, `provider:model`, resolved by
    /// [`SharedDeps::models`](crate::SharedDeps::models) before every model
    /// call. Becomes [`Agent::model_spec`].
    pub fn model_spec(&self) -> &str {
        &self.model_spec
    }

    /// The reasoning effort to ask for; None sends none and the model uses
    /// its default. Becomes [`Agent::effort`].
    pub fn effort(&self) -> Option<Effort> {
        self.effort
    }

    /// The history to resume from; empty for a new conversation. Becomes
    /// [`Agent::history`].
    pub fn history(&self) -> &[Message] {
        &self.history
    }
}

/// One conversation with a model: its history, the model it talks to, and
/// what it runs on. [`Agent::run`] runs one human turn.
///
/// An agent runs one turn at a time (`run` takes `&mut self`). Between turns
/// a host reads [`history`](Self::history), [`model_spec`](Self::model_spec)
/// and [`effort`](Self::effort), and changes them through
/// [`replace_history`](Self::replace_history),
/// [`set_model_spec`](Self::set_model_spec) and [`set_effort`](Self::set_effort).
/// A host that wraps the agent (haest-runtime's `Runtime`) keeps the agent to
/// itself, so its own record of the model can never drift from the agent's.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, SharedDeps, ShutWorkspace, StopReason, Tools,
///     testing::{ScriptedProvider, text_answer},
/// };
///
/// // A fake model that answers "hello"; a host resolves real providers here.
/// let provider = ScriptedProvider::new([text_answer("hello")]);
/// let shared = SharedDeps::new(provider.resolver(), "/project", ShutWorkspace);
/// let deps = AgentDeps::new(shared, Tools::new(), "You are terse.");
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:model-1"));
///
/// let mut said = String::new();
/// let mut sink = |ev: LoopEvent| {
///     if let LoopEvent::TextDelta { text } = ev {
///         said.push_str(&text);
///     }
/// };
/// let end = agent.run("say hello", &mut sink).await;
///
/// assert_eq!(end.stop_reason, StopReason::EndTurn);
/// assert_eq!(said, "hello");
/// assert_eq!(agent.history().len(), 2); // the input, the answer
/// assert_eq!(provider.requests()[0].system, "You are terse.");
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
pub struct Agent {
    /// The conversation's history. During a turn the loop only appends to it
    /// (and an [`AgentHooks::before_model_call`](crate::AgentHooks::before_model_call)
    /// may replace it).
    history: Vec<Message>,
    /// The model spec (`provider:model`) resolved before every model call.
    model_spec: String,
    /// The reasoning effort to ask for, fitted to what the model takes before
    /// every call; None sends none.
    effort: Option<Effort>,
    deps: AgentDeps,
    control: AgentControl,
    /// Model calls made for this conversation outside the loop's own (the
    /// compaction summary in a hook) since the running turn last looked.
    side_spend: Usage,
    /// The turn's thresholds and which have fired, across turns: this agent's
    /// own, following [`SharedDeps::thresholds`](crate::SharedDeps::thresholds).
    thresholds: SharedWatch,
}

/// The model, the effort, how long the history is and whether a turn runs;
/// not the history itself, which can be long.
impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("model_spec", &self.model_spec)
            .field("effort", &self.effort)
            .field("history_len", &self.history.len())
            .field("control", &self.control)
            .field("deps", &self.deps)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// An idle agent with `deps` and `opts`. Nothing is sent until [`Agent::run`].
    pub fn new(deps: AgentDeps, opts: AgentOptions) -> Self {
        let thresholds = Watch::new(deps.shared().thresholds().clone());
        Self {
            history: opts.history,
            model_spec: opts.model_spec,
            effort: opts.effort,
            deps,
            control: AgentControl::new(),
            side_spend: Usage::default(),
            thresholds: SharedWatch::new(thresholds),
        }
    }

    /// The conversation's history, oldest first: what the next model call
    /// sends, less any media the model cannot take. A call a dropped turn left
    /// with no result is answered when the next turn or compaction starts.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// The model spec (`provider:model`) the next model call resolves.
    pub fn model_spec(&self) -> &str {
        &self.model_spec
    }

    /// Switch models from the next model call on. Nothing is resolved here:
    /// a spec that does not resolve ends the next turn with an error.
    pub fn set_model_spec(&mut self, spec: impl Into<String>) {
        self.model_spec = spec.into();
    }

    /// The reasoning effort the next model call asks for; None asks for none.
    pub fn effort(&self) -> Option<Effort> {
        self.effort
    }

    /// The effort from the next model call on; None sends none and the model
    /// uses its default.
    pub fn set_effort(&mut self, effort: Option<Effort>) {
        self.effort = effort;
    }

    /// A model call made for this conversation outside the loop (compaction's
    /// summary): it counts toward the running turn's caps before its next model call.
    pub(crate) fn spent_outside_the_loop(&mut self, usage: &Usage) {
        self.side_spend += usage;
    }

    /// What was spent outside the loop since the last look.
    pub(crate) fn take_side_spend(&mut self) -> Usage {
        std::mem::take(&mut self.side_spend)
    }

    /// What this agent was built with. Fixed for the agent's life; what sits
    /// behind its `Arc`s may change.
    pub fn deps(&self) -> &AgentDeps {
        &self.deps
    }

    /// A handle to steer or abort this agent's turns from elsewhere.
    pub fn control(&self) -> AgentControl {
        self.control.clone()
    }

    /// Replace history wholesale (compaction, a cleared conversation, an
    /// edit between turns). Does not fire [`on_message`](crate::AgentHooks::on_message).
    pub fn replace_history(&mut self, history: Vec<Message>) {
        self.history = history;
    }

    /// History to edit in place: compaction swaps a prefix for its summary.
    pub(crate) fn history_mut(&mut self) -> &mut Vec<Message> {
        &mut self.history
    }

    /// The deps changed by `f`, in a test (a hook or a model swapped in).
    #[cfg(test)]
    pub(crate) fn map_deps(&mut self, f: impl FnOnce(AgentDeps) -> AgentDeps) {
        self.deps = f(self.deps.clone());
    }

    /// One human turn. Every event goes to `sink` in order, the last one is
    /// [`crate::LoopEvent::TurnEnd`]; the return value is how that turn ended.
    /// Input that arrives meanwhile goes through [`AgentControl::steer`]; stop it
    /// with [`AgentControl::abort`], or drop this future (a `timeout`, a
    /// `select!`): a tool call the dropped turn left without a result is
    /// answered at the start of the next turn or compaction, as interrupted, the way
    /// [`replay`](crate::session::replay) answers one in a log.
    ///
    /// `input` becomes a user message, with whatever
    /// [`attach_media`](crate::AgentHooks::attach_media) finds in it. The turn
    /// then makes model calls until one answers without tools (its stop
    /// reason, [`StopReason::MaxTokens`] when the model's output limit cut
    /// it), a cap or the conversation budget stops it
    /// ([`StopReason::MaxTokens`], with [`TurnOutcome::limit`] saying which),
    /// a hook or the provider fails ([`StopReason::Error`]), or it is aborted.
    ///
    /// A provider error, an unknown model spec and a tool that fails or
    /// panics are values, not panics: the turn goes on, or ends with a
    /// `TurnOutcome` saying why. (A hook that panics is the host's own panic.)
    /// Called while a turn of this agent is already running (from inside a
    /// hook), it ends at once with [`StopReason::Error`] and touches nothing.
    ///
    /// `sink` is any [`EventSink`]: a `Vec<LoopEvent>`, a closure, a channel,
    /// the host's own type, or a `&mut dyn EventSink` it was handed.
    pub async fn run(&mut self, input: &str, sink: &mut (impl EventSink + ?Sized)) -> TurnOutcome {
        self.run_on(input, &mut Forward(sink)).await
    }

    /// [`run`](Self::run), compiled once.
    async fn run_on(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        let started = Instant::now();
        let cancel = match self.control.begin() {
            Ok(c) => c,
            Err(e) => {
                // The running turn's thresholds are its own: this guard has none.
                let mut turn = Turn::new(CancellationToken::new(), self.bare_guard(), sink);
                return turn.finish(TurnOutcome::error(StopReason::Error, e), started);
            }
        };
        let _running = Running(self.control.clone());
        self.answer_dropped_calls();
        // Spent between turns (a compaction the human asked for) belongs to no turn.
        self.take_side_spend();
        let mut turn = Turn::new(cancel, self.guard(), sink);
        self.add_user(input, &mut turn).await;
        let end = self.steps(&mut turn).await;
        turn.finish(end, started)
    }

    /// A fresh guard per turn: its caps and clock start over, and the
    /// thresholds that fire once a turn re-arm. Only for a turn that began.
    fn guard(&self) -> TurnGuard {
        // A threshold the host took out of its list may drop here, locked.
        self.thresholds.update(|watch| {
            watch.follow(self.deps.shared().thresholds());
            watch.next_turn();
        });
        self.bare_guard()
            .with_thresholds(self.thresholds.clone(), last_context_tokens(&self.history))
    }

    /// A guard that leaves the agent's thresholds alone: for a refused call,
    /// which must not re-arm the running turn's.
    fn bare_guard(&self) -> TurnGuard {
        TurnGuard::new(
            self.deps.shared().clock().clone(),
            self.deps.shared().ledger().cloned(),
            self.deps.shared().remedies().clone(),
        )
    }

    /// The hard caps for the current model spec; read fresh at every check.
    fn turn_caps(&self) -> Option<TurnCaps> {
        self.deps.shared().turn_caps().turn_caps(&self.model_spec)
    }

    /// Everything a model call sends ahead of the history; compaction reuses it.
    ///
    /// `model` is the provider's model id the spec resolved to
    /// ([`ResolvedModel::model`](crate::ResolvedModel::model)). Builds the
    /// system prompt and the tool list afresh.
    pub(crate) async fn request_prefix(&self, model: &str) -> RequestPrefix {
        build_request_prefix(&self.deps, &self.model_spec, model, self.effort).await
    }

    /// History as the current model may receive it (media it cannot take is withheld, with a note).
    ///
    /// `model` is the provider's model id, named in the note. Reports what was
    /// withheld to [`AgentHooks::on_media_withheld`](crate::AgentHooks::on_media_withheld).
    pub(crate) fn sendable(&self, messages: &[Message], model: &str) -> Vec<Message> {
        let accepts = self
            .deps
            .shared()
            .facts()
            .input_modalities(&self.model_spec);
        fit_for_model(
            messages,
            accepts.as_ref(),
            model,
            self.deps.shared().hooks().as_ref(),
        )
    }
}

#[cfg(test)]
mod tests;
