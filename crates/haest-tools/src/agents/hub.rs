//! THE AGENT HUB: every subagent of one conversation, and the post office
//! between them (the default [`AgentDirectory`]).

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;
use haest_core::{Tools, newest::Newest};
use tokio_util::sync::CancellationToken;

use super::{
    AgentDirectory, AgentError, AgentMessage, AgentRef, AgentToolOptions, MAIN_AGENT, MemberStart,
    SpawnInput, SpawnOutcome, SubagentCaps, SubagentEvent, SubagentParentRef, budget,
    members::{Members, Status, run_id},
    run::Run,
    tools::agent_tools,
};

mod member_run;
mod post;
mod settle;

use member_run::Seat;
use settle::{Open, Work};

/// Every subagent of one conversation, run in this process: the default
/// [`AgentDirectory`].
///
/// A spawn checks the caps (depth, and how many run at once across the tree),
/// takes a unique name, and builds an [`Agent`] from the parent's
/// [`deps`](super::SubagentParent::deps) with its own tools (the parent's
/// [`subagent_tools`](super::SubagentParent::subagent_tools) for its kind minus the
/// task tools, plus its own agent tools), its own system prompt (the parent's, then
/// [`subagent_note`](super::subagent_note)) and its own turn caps. It runs one
/// turn; its final answer is the result.
///
/// - Before its loop exists, the parent's
///   [`start_member`](super::SubagentParent::start_member) runs it here (the
///   default) or on a run of the host's own, or refuses the spawn. That run
///   ([`MemberRun`](super::MemberRun)) may name the member's model spec, caps and
///   deps, and its [`end`](super::MemberRun::end) hears how the member ended,
///   its answer and its spend before the answer goes back.
/// - A foreground agent stops with the caller's turn; a background one runs
///   on, and its answer is steered into the subagent that started it, or
///   handed to the parent's [`submit_answer`](crate::Inbox::submit_answer)
///   when main started it or its starter has finished.
///   [`idle`](Self::idle) says when nothing is in flight, every answer
///   handed back (an agent leaves [`running`](Self::running) before that);
///   [`settled`](Self::settled) waits for it.
/// - A message ([`deliver`](AgentDirectory::deliver), an agent's or the person's, as
///   its [`Sender`](super::Sender) says) lands at the receiver's next step; one
///   for "main" goes to the parent's inbox. Messages that reach an agent after
///   it finished are appended to its answer, unread.
/// - A finished agent's name stays taken, so a message for it is refused, never
///   delivered to a newcomer.
/// - A crash inside a subagent comes back as its answer, never as a panic.
///
/// A hub is a cheap handle: clones share the subagents. The agent tools it
/// hands out ([`tools_for`](Self::tools_for)) hold a clone.
///
/// # Panics
///
/// A spawn outside a tokio runtime: every subagent runs on a task of its own.
///
/// # Examples
///
/// ```
/// use async_trait::async_trait;
/// use haest_core::{
///     SharedDeps, ToolRef, Tools,
///     caps::SubagentCaps,
///     provider::Effort,
///     // haest-core's `testing` feature: a provider that answers from a script.
///     testing::{ScriptedProvider, text_answer},
///     workspace::ShutWorkspace,
/// };
/// use haest_tools::{AgentDirectory, AgentRef, Inbox, SpawnInput, SubagentHub, SubagentParent};
/// use tokio_util::sync::CancellationToken;
///
/// struct Parent(ScriptedProvider);
///
/// impl Inbox for Parent {
///     fn submit(&self, text: String) {
///         println!("for main: {text}");
///     }
/// }
///
/// #[async_trait]
/// impl SubagentParent for Parent {
///     fn deps(&self) -> SharedDeps {
///         SharedDeps::new(self.0.resolver(), "/", ShutWorkspace)
///     }
///     fn base_tools(&self) -> Tools {
///         Tools::new()
///     }
///     async fn system_prompt(&self, _model: &str, _tools: &[ToolRef]) -> String {
///         "You are a helper.".into()
///     }
///     fn default_model(&self) -> (String, Option<Effort>) {
///         ("scripted:m".into(), None)
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let provider = ScriptedProvider::new([text_answer("42 files")]);
/// let hub = SubagentHub::new(Parent(provider), SubagentCaps::default());
///
/// // Main's agent tools; a host adds them to main's tool list.
/// let tools = hub.tools_for(&AgentRef::main());
/// let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
/// assert_eq!(names, ["Agent", "SendMessage", "StopAgent"]);
///
/// // What the Agent tool does: one subagent turn, its final answer back.
/// let input = SpawnInput::new("How many files are there?").with_description("count files");
/// let out = hub.spawn(&AgentRef::main(), input, &CancellationToken::new()).await?;
/// assert_eq!(out.text, "42 files");
/// assert!(hub.running().is_empty());
/// # Ok(())
/// # }
/// ```
///
/// [`Agent`]: haest_core::Agent
#[derive(Clone)]
pub struct SubagentHub(Arc<Inner>);

/// What a hub's clones share.
struct Inner {
    parent: SubagentParentRef,
    caps: SubagentCaps,
    run_id: String,
    members: Mutex<Members>,
    /// The running set after the latest start or end, for the parent's
    /// `on_activity`.
    activity: Newest<Vec<String>>,
    /// Spawns in flight, for [`SubagentHub::idle`] and [`SubagentHub::settled`].
    work: Arc<Work>,
}

impl SubagentHub {
    /// A hub with no subagents yet, over `parent` (the parent itself, or an
    /// `Arc` of one the host keeps too), under `caps`.
    pub fn new(parent: impl Into<SubagentParentRef>, caps: SubagentCaps) -> Self {
        Self(Arc::new(Inner {
            parent: parent.into(),
            caps,
            run_id: run_id(),
            members: Mutex::default(),
            activity: Newest::new(),
            work: Arc::default(),
        }))
    }

    /// The members' table, held until the guard drops. Never held while the
    /// parent's code or a member runs, so a poisoned one is a bug here.
    fn lock_members(&self) -> MutexGuard<'_, Members> {
        self.0
            .members
            .lock()
            .expect("the subagent hub's members lock poisoned")
    }

    /// Names of the subagents running now.
    pub fn running(&self) -> Vec<String> {
        let members = self.lock_members();
        members.running().map(|m| m.me.name.clone()).collect()
    }

    /// The agent tools `me` gets: Agent only while it may still nest (and
    /// `max_concurrent` is not 0), SendMessage and StopAgent always. The Agent
    /// tool offers the parent's kinds of subagent
    /// ([`SubagentParent::agent_tool_options`](super::SubagentParent::agent_tool_options)),
    /// and a `budget` when [`SubagentCaps::budgets`] is on.
    pub fn tools_for(&self, me: &AgentRef) -> Tools {
        let caps = &self.0.caps;
        let opts = AgentToolOptions {
            can_spawn: me.depth < caps.max_depth && caps.max_concurrent > 0,
            budget_caps: caps.budgets.then(|| self.budget_caps()),
            ..self.0.parent.agent_tool_options()
        };
        agent_tools(self.clone(), me.clone(), opts)
    }

    /// Stop every running subagent, as StopAgent does for one. A background
    /// one's answer still arrives, saying it stopped early.
    pub fn stop_all(&self) {
        for name in self.running() {
            self.stop(&name);
        }
    }

    /// A handle that does not keep the hub alive, for a test's parent that
    /// looks at its own hub.
    #[cfg(test)]
    pub(crate) fn downgrade(&self) -> WeakHub {
        WeakHub(Arc::downgrade(&self.0))
    }
}

/// A [`SubagentHub`] that does not keep it alive ([`SubagentHub::downgrade`]).
#[cfg(test)]
pub(crate) struct WeakHub(std::sync::Weak<Inner>);

#[cfg(test)]
impl WeakHub {
    pub(crate) fn upgrade(&self) -> Option<SubagentHub> {
        self.0.upgrade().map(SubagentHub)
    }
}

/// What the agent tools speak to.
#[async_trait]
impl AgentDirectory for SubagentHub {
    /// Agent: run a subagent for `from`. `cancel` is the caller's turn: a
    /// foreground agent stops with it.
    ///
    /// Cancel safe: dropping the future (a host's timeout or `select!`)
    /// while the parent's start runs forgets the member, its name free
    /// again; dropping it while a foreground member runs stops the member
    /// as `cancel` does, and its run still ends. A panic in the parent's
    /// start forgets it too. Either way its `max_concurrent` place comes back.
    ///
    /// # Errors
    ///
    /// An [`AgentError`] the model corrects: a cap ([`AgentError::TooDeep`],
    /// [`AgentError::TooMany`], [`AgentError::OverBudget`]), or the parent's
    /// refusal from [`start_member`](super::SubagentParent::start_member).
    async fn spawn(
        &self,
        from: &AgentRef,
        input: SpawnInput,
        cancel: &CancellationToken,
    ) -> Result<SpawnOutcome, AgentError> {
        // In flight until its answer is back, however it ends (refused too).
        let open = Open::begin(&self.0.work);
        // A budget only tightens, whoever built the input; its USD is held
        // first, released when the member ends (or never starts).
        let budget = (input.budget).map(|b| budget::clamp(b, &self.budget_caps()));
        let held = match &budget {
            Some(b) => budget::hold(self.0.parent.deps().ledger(), b)?,
            None => None,
        };
        let run = {
            let mut members = self.lock_members();
            let m = members.reserve(from, &input, &self.0.caps, &self.0.run_id)?;
            Run {
                me: m.me.clone(),
                id: m.id.clone(),
                parent: from.name.clone(),
                description: input.description.clone(),
                stop: m.stop.clone(),
            }
        };
        // Given back however this spawn or the member's run stops.
        let seat = Seat::reserved(self, &run.me.name);
        // The parent is asked with the table unlocked: it may call the hub back.
        let (main_spec, effort) = self.0.parent.default_model();
        let start = MemberStart {
            name: run.me.name.clone(),
            id: run.id.clone(),
            parent: run.parent.clone(),
            depth: run.me.depth,
            model_spec: input.model_spec.clone().unwrap_or(main_spec),
            caps: self.member_caps(budget.as_ref()),
            input: SpawnInput { budget, ..input },
        };
        // A refusal, a panic in the host's start or this future dropped while
        // it runs: the seat is forgotten, the name free again.
        let member = self.0.parent.start_member(&start, cancel).await?;
        let agent = self.build(&run, &start, effort, &member);
        let name = run.me.name.clone();
        self.lock_members().attach(&name, agent.control());
        let seat = seat.started();
        let prompt = start.input.prompt;
        let hub = self.clone();
        if !start.input.background {
            // On a task of its own, so a caller that drops this future (a
            // timeout, a select!) stops the member as a cancelled caller
            // does: its run still ends, its end heard and its seat given back.
            let caller = cancel.child_token();
            let _stop_when_dropped = caller.clone().drop_guard();
            let task = tokio::spawn(async move {
                let _open = open;
                let out = (hub.run_member(seat, run, agent, member, prompt, Some(caller))).await;
                drop(held);
                out
            });
            return match task.await {
                Ok(out) => Ok(out),
                // A host callback panicked under it: the caller's panic, as
                // when the member ran on the caller's task.
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                // The runtime is shutting down.
                Err(_) => Ok(SpawnOutcome {
                    text: "(no answer)\n\n[the agent stopped early: its runtime shut down]".into(),
                    cost_usd: 0.0,
                }),
            };
        }
        let text = format!(
            "Started agent \"{name}\" ({}) in the background. Its answer will arrive as a message; SendMessage reaches it while it runs.",
            agent.model_spec()
        );
        tokio::spawn(async move {
            // Dropped last: the hub is idle only once the answer is handed back.
            let _open = open;
            let parent = run.parent.clone();
            let out = hub.run_member(seat, run, agent, member, prompt, None).await;
            // Its USD is free again before its answer is read.
            drop(held);
            hub.hand_back(
                &parent,
                format!("[Agent \"{name}\" finished]\n\n{}", out.text),
            );
        });
        Ok(SpawnOutcome {
            text,
            cost_usd: 0.0,
        })
    }

    /// Hand `message` to the agent it is for, main or a running subagent,
    /// and say what to tell its sender. A subagent reads it at its next
    /// step; one for main goes to the parent's
    /// [`Inbox::submit`](crate::Inbox::submit). The receiver reads who wrote
    /// above the text: SendMessage sends from
    /// [`Sender::Agent`](super::Sender::Agent), and a host lets the person
    /// write with [`Sender::Person`](super::Sender::Person).
    ///
    /// Every delivery tells the parent ([`SubagentEvent::Message`]); a
    /// subagent that has finished, or never ran here, refuses the message and
    /// nothing is told.
    ///
    /// # Errors
    ///
    /// [`AgentError::Finished`] or [`AgentError::UnknownAgent`], with who a
    /// message can reach now.
    fn deliver(&self, message: AgentMessage) -> Result<String, AgentError> {
        let AgentMessage { from, to, text } = &message;
        let body = format!("{}\n\n{text}", from.header());
        let (id, said) = if to == MAIN_AGENT {
            self.0.parent.submit(body);
            (self.id_of(from), "Delivered to main.".to_string())
        } else {
            let mut members = self.lock_members();
            let Some(m) = members.get_mut(to).filter(|m| m.status == Status::Running) else {
                let (name, running) = (to.to_string(), members.reachable());
                return Err(if members.known(to) {
                    AgentError::Finished { name, running }
                } else {
                    AgentError::UnknownAgent { name, running }
                });
            };
            m.steer(body);
            let said = format!("Delivered to \"{to}\"; it reads it at its next step.");
            (m.id.clone(), said)
        };
        self.0
            .parent
            .on_subagent(&id, SubagentEvent::Message(message));
        Ok(said)
    }

    /// StopAgent: stop the subagent `name`; false when it is not running.
    fn stop(&self, name: &str) -> bool {
        let mut members = self.lock_members();
        match members.get_mut(name) {
            Some(m) if m.status == Status::Running => {
                m.status = Status::Stopped;
                m.abort();
                true
            }
            _ => false,
        }
    }
}

/// The running subagents by name, and the caps.
impl fmt::Debug for SubagentHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubagentHub")
            .field("running", &self.running())
            .field("caps", &self.0.caps)
            .finish_non_exhaustive()
    }
}
