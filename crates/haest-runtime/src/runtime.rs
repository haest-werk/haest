//! A built conversation ([`Runtime`]): its turns, what a host changes on it
//! while it runs (the model, the effort, a fresh session, the skill list) and
//! its end. Each change goes to the agent, the
//! [`RuntimeHooks`](crate::RuntimeHooks) and the session file together; the
//! hooks' own setters are crate-private, so these are the only way to make
//! one all three see.

use std::{fmt, sync::Arc};

use tokio_util::sync::CancellationToken;

use crate::{
    RuntimeHooks, SessionError,
    haest::{
        Agent, Effort, EventSink, ModelError, TurnOutcome,
        core::compaction::CompactionError,
        core::fs::FsRef,
        core::session::SessionRecord,
        core::skills::{Skill, SkillScanRoots, Skills, skills_in},
        core::task_board::TaskBoard,
        mcp::McpHub,
        mods::{ApprovalGate, Mods},
        now_ms,
        tools::{JobHub, SubagentHub},
    },
    hooks::Compact,
    inbox::InboxQueue,
    store::{new_session, record_start},
    world::kill_jobs,
};

/// A built conversation: the agent, and everything it runs on.
///
/// [`build`](crate::build) returns one. The host runs the human's turns with
/// [`human_turn`](Self::human_turn), then every turn the inbox holds
/// ([`next_agent_turn`](Self::next_agent_turn), run with [`run`](Self::run));
/// changes the model, the effort, the session or the skills through the
/// methods here, which keep the agent, the hooks and the session file in step;
/// and ends with [`shut_down`](Self::shut_down). The crate docs show a host's
/// turn loop.
///
/// `U` is the host's own [`Ui`](crate::Ui) type, so [`ui`](Self::ui) gives it
/// back as the host made it.
///
/// Every part it runs on is read with the getter of its name. The services
/// among them ([`hooks`](Self::hooks), [`mcp`](Self::mcp),
/// [`subagents`](Self::subagents), [`jobs`](Self::jobs), ...) are cheap
/// handles: a host that hands the runtime on (to a session) clones the ones
/// it still needs first.
pub struct Runtime<U> {
    /// The main agent. Only the methods here change it, so the hooks and the
    /// session file always know its model and effort.
    pub(crate) agent: Agent,
    pub(crate) hooks: RuntimeHooks,
    pub(crate) ui: Arc<U>,
    pub(crate) fs: FsRef,
    pub(crate) board: TaskBoard,
    pub(crate) skills: Skills,
    /// Where the skills were found at the start; the sandbox mounts these.
    pub(crate) skill_roots: SkillScanRoots,
    pub(crate) mcp: McpHub,
    pub(crate) mods: Mods,
    pub(crate) gate: ApprovalGate,
    pub(crate) inbox: InboxQueue,
    pub(crate) subagents: SubagentHub,
    pub(crate) jobs: Option<JobHub>,
}

/// The parts it runs on; the host's UI is left out.
impl<U> fmt::Debug for Runtime<U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("agent", &self.agent)
            .field("hooks", &self.hooks)
            .field("board", &self.board)
            .field("skills", &self.skills)
            .field("mcp", &self.mcp)
            .field("mods", &self.mods)
            .field("gate", &self.gate)
            .field("inbox", &self.inbox)
            .field("subagents", &self.subagents)
            .field("jobs", &self.jobs)
            .finish_non_exhaustive()
    }
}

impl<U> Runtime<U> {
    /// The main agent, to read: its history, model, effort and deps, and
    /// [`Agent::control`] to steer or stop the turn running.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// What the agent reports to: the session file, the [`Ui`](crate::Ui),
    /// the current model and effort, compaction.
    pub fn hooks(&self) -> &RuntimeHooks {
        &self.hooks
    }

    /// The host's own UI, as it handed it to [`build`](crate::build).
    pub fn ui(&self) -> &U {
        &self.ui
    }

    /// Where the conversation reads and writes: the world's
    /// [`fs`](crate::World::fs).
    pub fn fs(&self) -> &FsRef {
        &self.fs
    }

    /// The task list, saved to the session file on every change.
    pub fn board(&self) -> &TaskBoard {
        &self.board
    }

    /// The skills offered now: the Skill tool and the system prompt read
    /// this list. [`rescan_skills`](Self::rescan_skills) reads the skill
    /// dirs again; [`Skills::replace`] puts a list of the host's own.
    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    /// The connected MCP servers. [`shut_down`](Self::shut_down) closes them.
    pub fn mcp(&self) -> &McpHub {
        &self.mcp
    }

    /// The mods turned on: the approval mod, plan mode, the human's and the
    /// project's. [`human_turn`](Self::human_turn) passes the human's text
    /// through their `rewrite_prompt`; a host runs their commands.
    pub fn mods(&self) -> &Mods {
        &self.mods
    }

    /// The human's Allow / Deny every mod's ask goes through, with the
    /// ALWAYS answers given so far; [`revoke`](ApprovalGate::revoke) takes
    /// a tool's back.
    pub fn gate(&self) -> &ApprovalGate {
        &self.gate
    }

    /// Turns the human did not start, waiting for main.
    pub fn inbox(&self) -> &InboxQueue {
        &self.inbox
    }

    /// The same, for a host that runs its own turn loop over it.
    pub fn inbox_mut(&mut self) -> &mut InboxQueue {
        &mut self.inbox
    }

    /// Main's subagents (none run when the `agents` setting is off).
    /// [`shut_down`](Self::shut_down) stops them.
    pub fn subagents(&self) -> &SubagentHub {
        &self.subagents
    }

    /// Background shell jobs; None unless Bash runs in the real workspace.
    /// [`shut_down`](Self::shut_down) kills them, and so does
    /// [`kill_jobs`](crate::kill_jobs).
    pub fn jobs(&self) -> Option<&JobHub> {
        self.jobs.as_ref()
    }

    /// A turn the human started: `text` through the mods' prompt rewrite
    /// (`prompt.submit`), with what the inbox's gate held riding in ahead of
    /// it ([`InboxQueue::human`]), then [`run`](Self::run).
    pub async fn human_turn(&mut self, text: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        let text = self.mods.rewrite_prompt(text).await;
        let text = self.inbox.human(&text);
        self.run(&text, sink).await
    }

    /// The input of the next turn the human did not start (a background job
    /// that exited, a subagent's message for main), through the gate. None
    /// when nothing waits, or when the gate holds what does: it then rides in
    /// with the human's next turn. Run each with [`run`](Self::run) until
    /// this is None.
    pub fn next_agent_turn(&mut self) -> Option<String> {
        let control = self.agent.control();
        loop {
            let waiting = self.inbox.take(&control)?;
            if let Some(text) = self.inbox.agent_turn(&waiting) {
                return Some(text);
            }
        }
    }

    /// One turn of main on `input` as it is, every event to `sink`: what
    /// reaches the [`inbox`](Self::inbox) while it runs is steered into it,
    /// and the session file is flushed when it ends.
    pub async fn run(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        let control = self.agent.control();
        let end = {
            let run = self.agent.run(input, sink);
            tokio::pin!(run);
            loop {
                tokio::select! {
                    end = &mut run => break end,
                    Some(text) = self.inbox.arrival() => control.steer(text),
                }
            }
        };
        self.hooks.flush().await;
        end
    }

    /// The end of the conversation: the subagents stopped, the background
    /// jobs killed (and waited for, at most 3 seconds, so none outlives the
    /// host), the MCP servers closed and the session file flushed. Returns
    /// what was still running, for the host to tell the human.
    pub async fn shut_down(&self) -> ShutDown {
        let subagents = self.subagents.running().len();
        self.subagents.stop_all();
        let jobs = match &self.jobs {
            Some(jobs) => kill_jobs(jobs).await,
            None => 0,
        };
        self.mcp.close().await;
        self.hooks.flush().await;
        ShutDown { subagents, jobs }
    }

    /// Compact main's conversation now (the human's /compact): the older
    /// messages become one summary, the latest are kept, and the session file
    /// records it. What the summary calls cost goes on the conversation's
    /// spend either way. A conversation too short to cut says so to the
    /// [`Ui`](crate::Ui). `cancel` stops the summary call.
    ///
    /// # Errors
    ///
    /// When no summary came out: the history is as it was. The error is said
    /// to the [`Ui`](crate::Ui) already, unless `cancel` fired.
    pub async fn compact(&mut self, cancel: &CancellationToken) -> Result<(), CompactionError> {
        let by = Compact::Manual;
        self.hooks.compact_now(&mut self.agent, cancel, by).await
    }

    /// The id of the session file records go to now; [`clear`](Self::clear)
    /// changes it. A host shows it so the human can resume the session.
    pub fn session_id(&self) -> String {
        self.hooks.store().session_id().to_string()
    }

    /// Main's model from the next model call on, and what a subagent starts
    /// on from now. The session file records it, so a resume keeps it.
    ///
    /// # Errors
    ///
    /// When `spec` does not resolve (an unknown provider, no key for it):
    /// then nothing changes.
    pub fn set_model_spec(&mut self, spec: &str) -> Result<(), ModelError> {
        self.agent.deps().shared().models().resolve(spec)?;
        self.agent.set_model_spec(spec);
        self.hooks.set_model_spec(spec);
        self.hooks.append(SessionRecord::Config {
            t: now_ms(),
            model_spec: Some(spec.to_string()),
            effort: None,
        });
        Ok(())
    }

    /// Main's effort from the next call on (None = the model's default), and
    /// what a subagent starts on from now; recorded like the model. A model
    /// the catalog says does not take this effort is sent the lowest one it
    /// takes, and the [`Ui`](crate::Ui) is told once.
    pub fn set_effort(&mut self, effort: Option<Effort>) {
        self.agent.set_effort(effort);
        self.hooks.set_effort(effort);
        self.hooks.append(SessionRecord::Config {
            t: now_ms(),
            model_spec: None,
            effort: Some(effort),
        });
    }

    /// A fresh session (the human's /clear): new id, new file, empty history
    /// and task list, on the same model and effort; the ledger (the spend so
    /// far) carries over. The old session stays on disk and still resumes.
    ///
    /// # Errors
    ///
    /// When the new session's file cannot be created. It is created, header
    /// first, before anything switches to it, so then nothing changes.
    pub async fn clear(&mut self) -> Result<(), SessionError> {
        let ui = self.hooks.ui().clone();
        let store = new_session(&self.fs, self.hooks.settings(), ui).await?;
        self.hooks.swap_store(store);
        self.agent.replace_history(Vec::new());
        self.board.restore(Vec::new());
        record_start(&self.hooks, &self.agent);
        Ok(())
    }

    /// The skill dirs found at the start, read again: a skill added to one
    /// is offered from now on. A plugin installed since the start is not (its
    /// dir is not in the sandbox); a new conversation picks it up.
    ///
    /// Returns the skills found, which [`skills`](Self::skills) now holds.
    pub async fn rescan_skills(&self) -> Vec<Skill> {
        let found = skills_in(self.fs.as_ref(), &self.skill_roots).await;
        self.skills.replace(found.clone());
        found
    }
}

/// What [`Runtime::shut_down`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutDown {
    /// Subagents that were still running.
    pub subagents: usize,
    /// Background jobs that were still running, now killed.
    pub jobs: usize,
}
