//! THE AGENT HUB: every subagent of one conversation, and the post office
//! between them (the default [`AgentDirectory`]).

use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use haest_core::{Agent, AgentDeps, AgentOptions, tool::Tool};
use tokio_util::sync::CancellationToken;

use super::{
    AgentDirectory, AgentError, AgentRef, MAIN, SpawnInput, SpawnOutcome, SubagentCaps,
    SubagentEvent, SubagentParent,
    members::{Due, Members, Status, run_id},
    run::{ChildFacts, ChildHooks, ChildPrompt, Run, drive, subagent_note},
    tools::agent_tools,
};

/// Every subagent of one conversation, run in this process: the default
/// [`AgentDirectory`].
///
/// A spawn checks the caps (depth, and how many run at once across the tree),
/// takes a unique name, and builds an [`Agent`] from the parent's
/// [`deps`](SubagentParent::deps) with its own tools (the parent's
/// [`base_tools`](SubagentParent::base_tools) minus the task tools, plus its own
/// agent tools), its own system prompt (the parent's, then
/// [`subagent_note`](super::subagent_note)) and its own turn caps. It runs one
/// turn; its final answer is the result.
///
/// - A foreground agent stops with the caller's turn; a background one runs
///   on, and its answer is steered into the subagent that started it, or
///   submitted to the parent's inbox when main started it or its starter has
///   finished. [`answers_due`](Self::answers_due) counts the ones on their way.
/// - A message ([`deliver`](AgentDirectory::deliver)) lands at the receiver's
///   next step; one for "main" goes to the parent's inbox. Messages that reach an
///   agent after it finished are appended to its answer, unread.
/// - A finished agent's name stays taken, so a message for it is refused, never
///   delivered to a newcomer.
/// - A crash inside a subagent comes back as its answer, never as a panic.
///
/// # Panics
///
/// A background spawn outside a tokio runtime: the agent runs on a task of
/// its own.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use async_trait::async_trait;
/// use haest_core::{
///     AgentDeps,
///     provider::Effort,
///     // haest-core's `testing` feature: a provider that answers from a script.
///     testing::{ScriptedProvider, text_answer},
///     tool::Tool,
///     workspace::ShutWorkspace,
/// };
/// use haest_tools::{
///     Inbox, SubagentParent, SubagentHub,
///     agents::{AgentDirectory, AgentRef, SpawnInput, SubagentCaps},
/// };
/// use tokio_util::sync::CancellationToken;
///
/// struct Parent(Arc<ScriptedProvider>);
///
/// impl Inbox for Parent {
///     fn submit(&self, text: String) {
///         println!("for main: {text}");
///     }
/// }
///
/// #[async_trait]
/// impl SubagentParent for Parent {
///     fn deps(&self) -> AgentDeps {
///         let models = Arc::new(self.0.resolver());
///         let prompt = Arc::new("You are a helper.".to_string());
///         AgentDeps::new(models, Arc::new(self.base_tools()), prompt, "/", Arc::new(ShutWorkspace))
///     }
///     fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
///         Vec::new()
///     }
///     async fn system_prompt(&self, _model: &str, _tools: &[Arc<dyn Tool>]) -> String {
///         "You are a helper.".into()
///     }
///     fn default_model(&self) -> (String, Option<Effort>) {
///         ("scripted:m".into(), None)
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let provider = ScriptedProvider::new([text_answer("42 files")]);
/// let hub = SubagentHub::new(Arc::new(Parent(provider)), SubagentCaps::default());
///
/// // Main's agent tools; a host adds them to main's tool list.
/// let tools = hub.tools_for(&AgentRef::main());
/// let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
/// assert_eq!(names, ["Agent", "SendMessage", "StopAgent"]);
///
/// // What the Agent tool does: one subagent turn, its final answer back.
/// let input = SpawnInput {
///     description: "count files".into(),
///     prompt: "How many files are there?".into(),
///     ..Default::default()
/// };
/// let out = hub.spawn(&AgentRef::main(), input, CancellationToken::new()).await.unwrap();
/// assert_eq!(out.text, "42 files");
/// assert!(hub.running().is_empty());
/// # }
/// ```
///
/// [`Agent`]: haest_core::Agent
pub struct SubagentHub {
    parent: Arc<dyn SubagentParent>,
    caps: SubagentCaps,
    run_id: String,
    members: Mutex<Members>,
    /// Background agents whose answer has not been handed back yet.
    due: Arc<AtomicUsize>,
    this: Weak<SubagentHub>,
}

impl SubagentHub {
    /// A hub with no subagents yet, over `parent`, under `caps`.
    pub fn new(parent: Arc<dyn SubagentParent>, caps: SubagentCaps) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            parent,
            caps,
            run_id: run_id(),
            members: Mutex::default(),
            due: Arc::default(),
            this: this.clone(),
        })
    }

    /// Background agents whose answer has not reached its reader yet: running,
    /// or finishing. A host that waits for background work (a one-shot run
    /// before it exits) waits on this, never on [`Self::running`]: an agent
    /// leaves the running set BEFORE its answer is handed back.
    pub fn answers_due(&self) -> usize {
        self.due.load(Ordering::SeqCst)
    }

    fn arc(&self) -> Arc<Self> {
        self.this
            .upgrade()
            .expect("a hub is alive while it is called")
    }

    /// Names of the subagents running now.
    pub fn running(&self) -> Vec<String> {
        let members = self.members.lock().unwrap();
        members.running().map(|m| m.me.name.clone()).collect()
    }

    /// The agent tools `me` gets: Agent only while it may still nest (and
    /// `max_concurrent` is not 0), SendMessage and StopAgent always.
    pub fn tools_for(&self, me: &AgentRef) -> Vec<Arc<dyn Tool>> {
        let can_spawn = me.depth < self.caps.max_depth && self.caps.max_concurrent > 0;
        agent_tools(self.arc(), me.clone(), can_spawn)
    }

    /// Stop every running subagent, as StopAgent does for one. A background
    /// one's answer still arrives, saying it stopped early.
    pub fn stop_all(&self) {
        for name in self.running() {
            self.stop(&name);
        }
    }

    fn report_activity(&self) {
        let ids: Vec<String> = {
            let members = self.members.lock().unwrap();
            members.running().map(|m| m.id.clone()).collect()
        };
        self.parent.on_activity(&ids);
    }

    /// The agent for `run`: the conversation's deps with its own tools, prompt
    /// and hooks. No task tools: the task list belongs to the conversation, not
    /// to a helper.
    fn build(&self, run: &Run, model: Option<String>) -> Agent {
        let mut tools: Vec<Arc<dyn Tool>> = self
            .parent
            .base_tools()
            .into_iter()
            .filter(|t| crate::builtin_word(t.name()) != Some("tasks"))
            .collect();
        tools.extend(self.tools_for(&run.me));
        let source = self.parent.tool_source(tools);
        let (main_model, effort) = self.parent.default_model();
        let model = model.unwrap_or(main_model);
        let base = self.parent.deps();
        let deps = AgentDeps {
            tools: source.clone(),
            system_prompt: Arc::new(ChildPrompt {
                parent: self.parent.clone(),
                model: model.clone(),
                tools: source,
                note: subagent_note(&run.me.name, &run.parent, &run.description, &self.caps),
            }),
            hooks: Arc::new(ChildHooks {
                hooks: base.hooks.clone(),
                parent: self.parent.clone(),
                id: run.id.clone(),
            }),
            facts: Arc::new(ChildFacts {
                facts: base.facts.clone(),
                caps: self.caps,
            }),
            ..base
        };
        let mut opts = AgentOptions::new(model);
        opts.effort = effort;
        Agent::new(deps, opts)
    }

    async fn run_member(
        &self,
        run: Run,
        mut agent: Agent,
        prompt: String,
        caller: Option<CancellationToken>,
    ) -> SpawnOutcome {
        self.report_activity();
        let started = SubagentEvent::Started {
            name: run.me.name.clone(),
            parent: run.parent.clone(),
            description: run.description.clone(),
        };
        self.parent.on_subagent(&run.id, started);
        let parent = self.parent.as_ref();
        let out = drive(
            &mut agent,
            &prompt,
            &run.stop,
            caller.as_ref(),
            parent,
            &run.id,
        )
        .await;
        self.parent
            .on_subagent(&run.id, SubagentEvent::Stopped { ok: out.ok });
        // Leave first: from here a message for it is refused, not lost.
        self.members.lock().unwrap().leave(&run.me.name);
        let late = agent.control().take_queued();
        self.report_activity();
        let unread = late.map_or_else(String::new, |late| {
            format!(
                "\n\n[messages that reached \"{}\" after it finished, unread:]\n{late}",
                run.me.name
            )
        });
        SpawnOutcome {
            text: format!("{}{unread}", out.text),
            cost_usd: out.cost_usd,
        }
    }

    /// A background agent's answer goes to whoever started it -- or to main when
    /// that one has finished meanwhile. Never fails: an answer must not be lost to a race.
    fn hand_back(&self, parent: &str, text: String) {
        let mut members = self.members.lock().unwrap();
        match members.get_mut(parent) {
            Some(p) if p.status == Status::Running => p.steer(text),
            _ => {
                drop(members);
                self.parent.submit(text);
            }
        }
    }
}

#[async_trait]
impl AgentDirectory for SubagentHub {
    async fn spawn(
        &self,
        from: &AgentRef,
        input: SpawnInput,
        cancel: CancellationToken,
    ) -> Result<SpawnOutcome, AgentError> {
        let run = {
            let mut members = self.members.lock().unwrap();
            let m = members.reserve(from, &input, &self.caps, &self.run_id)?;
            Run {
                me: m.me.clone(),
                id: m.id.clone(),
                parent: from.name.clone(),
                description: input.description.clone(),
                stop: m.stop.clone(),
            }
        };
        // The parent is asked with the table unlocked: it may call the hub back.
        let agent = self.build(&run, input.model.clone());
        let name = run.me.name.clone();
        self.members.lock().unwrap().attach(&name, agent.control());
        if !input.background {
            return Ok(self
                .run_member(run, agent, input.prompt, Some(cancel))
                .await);
        }
        let text = format!(
            "Started agent \"{name}\" in the background. Its answer will arrive as a message; SendMessage reaches it while it runs."
        );
        let hub = self.arc();
        let due = Due::owe(&self.due);
        tokio::spawn(async move {
            // Dropped last: the answer is handed back before it stops being due.
            let _due = due;
            let parent = run.parent.clone();
            let out = hub.run_member(run, agent, input.prompt, None).await;
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

    fn deliver(&self, from: &str, to: &str, text: &str) -> Result<String, AgentError> {
        let body = format!("[Message from agent \"{from}\"]\n\n{text}");
        if to == MAIN {
            self.parent.submit(body);
            return Ok("Delivered to main.".into());
        }
        let mut members = self.members.lock().unwrap();
        if let Some(m) = members.get_mut(to).filter(|m| m.status == Status::Running) {
            m.steer(body);
            return Ok(format!(
                "Delivered to \"{to}\"; it reads it at its next step."
            ));
        }
        let (name, running) = (to.to_string(), members.reachable());
        Err(if members.known(to) {
            AgentError::Finished { name, running }
        } else {
            AgentError::UnknownAgent { name, running }
        })
    }

    fn stop(&self, name: &str) -> bool {
        let mut members = self.members.lock().unwrap();
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
