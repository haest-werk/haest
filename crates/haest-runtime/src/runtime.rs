//! A built conversation ([`Runtime`]): its turns, what a host changes on it
//! while it runs (the model, the effort, a fresh session, the skill list) and
//! its end. Each change goes to the agent, the
//! [`RuntimeHooks`](crate::RuntimeHooks) and the session file together; the
//! hooks' own setters are crate-private, so these are the only way to make
//! one all three see.

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::{
    RuntimeHooks, SessionError,
    haest::{
        Agent, Effort, EventSink, ModelError, TurnOutcome,
        core::compaction::CompactionError,
        core::fs::Fs,
        core::session::SessionRecord,
        core::skills::{Skill, SkillScanRoots, skills_in},
        core::task_board::TaskBoard,
        mcp::McpHub,
        mods::{ApprovalGate, Mods},
        now_ms,
        tools::{JobHub, SubagentHub},
    },
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
pub struct Runtime<U: ?Sized> {
    /// The main agent. Only the methods here change it, so the hooks and the
    /// session file always know its model and effort.
    pub(crate) agent: Agent,
    /// What the agent reports to: the session file, the [`Ui`](crate::Ui),
    /// the current model and effort, compaction.
    pub hooks: Arc<RuntimeHooks>,
    /// The host's own UI, as it handed it in.
    pub ui: Arc<U>,
    /// Where the conversation reads and writes: the world's
    /// [`Fs`](crate::World::fs).
    pub fs: Arc<dyn Fs>,
    /// The task list, saved to the session file on every change.
    pub board: Arc<TaskBoard>,
    /// The skills offered now: the Skill tool and the system prompt read
    /// this list ([`rescan_skills`](Self::rescan_skills) refreshes it).
    pub skills: Arc<Mutex<Vec<Skill>>>,
    /// Where the skills were found at the start; the sandbox mounts these.
    pub(crate) skill_roots: SkillScanRoots,
    /// The connected MCP servers; [`McpHub::close`] them at the end.
    pub mcp: Arc<McpHub>,
    /// The mods turned on: the approval mod, plan mode, the human's and the
    /// project's. A host passes the human's text through `rewrite_prompt`.
    pub mods: Arc<Mods>,
    /// The human's Allow / Deny every mod's ask goes through, with the
    /// ALWAYS answers given so far; [`revoke`](ApprovalGate::revoke) takes
    /// a tool's back.
    pub gate: ApprovalGate,
    /// Turns the human did not start, waiting for main.
    pub inbox: InboxQueue,
    /// Main's subagents (none run when `agents` is off); `stop_all` them at
    /// the end.
    pub subagents: Arc<SubagentHub>,
    /// Background shell jobs; None unless Bash runs in the real workspace.
    /// Kill them at the end ([`kill_jobs`](crate::kill_jobs)).
    pub jobs: Option<Arc<JobHub>>,
}

impl<U: ?Sized> Runtime<U> {
    /// The main agent, to read: its history, model, effort and deps, and
    /// [`Agent::control`] to steer or stop the turn running.
    pub fn agent(&self) -> &Agent {
        &self.agent
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

    /// Compact main's conversation now (the human's /compact): see
    /// [`RuntimeHooks::compact_now`](crate::RuntimeHooks::compact_now). A
    /// conversation too short to cut says so to the [`Ui`](crate::Ui).
    ///
    /// # Errors
    ///
    /// When no summary came out; the history is as it was.
    pub async fn compact(&mut self, cancel: &CancellationToken) -> Result<(), CompactionError> {
        self.hooks.compact_now(&mut self.agent, cancel, false).await
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
        self.agent.deps().shared.models.resolve(spec)?;
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
        let store = new_session(&self.fs, &self.hooks.settings, self.hooks.ui.clone()).await?;
        self.hooks.swap_store(Arc::new(store));
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
        *self.skills.lock().unwrap() = found.clone();
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
