//! A built conversation ([`Runtime`]), and what a host changes on it while it
//! runs: the model, the effort, a fresh session (/clear), the skill list. Each
//! change goes to the agent, the [`RuntimeHooks`](crate::RuntimeHooks) and the
//! session file together; the hooks' own setters are crate-private, so these
//! are the only way to make one all three see.

use std::sync::{Arc, Mutex};

use anyhow::anyhow;

use crate::{
    RuntimeHooks,
    haest::{
        Agent, Effort,
        core::fs::Fs,
        core::session::SessionRecord,
        core::skills::{Skill, SkillScanRoots, skills_in},
        core::task_board::TaskBoard,
        mcp::McpHub,
        mods::Mods,
        now_ms,
        tools::{JobHub, agents::SubagentHub},
    },
    inbox::InboxQueue,
    store::{new_session, record_start},
};

/// A built conversation: the agent, and everything it runs on.
///
/// [`build`](crate::build) returns one. The host runs turns with
/// [`agent`](Self::agent), feeds it what reaches the [`inbox`](Self::inbox),
/// and changes the model, the effort, the session or the skills through the
/// methods here, which keep the agent, the hooks and the session file in step.
/// The crate docs show a host's turn loop and its shutdown.
///
/// `U` is the host's own [`Ui`](crate::Ui) type, so [`ui`](Self::ui) gives it
/// back as the host made it.
pub struct Runtime<U: ?Sized> {
    /// The main agent: [`Agent::run`] runs one turn, [`Agent::control`] steers
    /// or stops the one running. Change its model and effort with
    /// [`set_model`](Self::set_model) and [`set_effort`](Self::set_effort),
    /// not its fields: only those tell the hooks and the session file.
    pub agent: Agent,
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
    /// Turns the human did not start, waiting for main.
    pub inbox: InboxQueue,
    /// Main's subagents (none run when `agents` is off); `stop_all` them at
    /// the end.
    pub agents: Arc<SubagentHub>,
    /// Background shell jobs; None unless Bash runs in the real workspace.
    /// Kill them at the end ([`kill_jobs`](crate::world::kill_jobs)).
    pub jobs: Option<Arc<JobHub>>,
}

impl<U: ?Sized> Runtime<U> {
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
    pub fn set_model(&mut self, spec: &str) -> anyhow::Result<()> {
        self.agent
            .deps()
            .models
            .resolve(spec)
            .map_err(|e| anyhow!(e))?;
        self.agent.model_spec = spec.to_string();
        self.hooks.set_model(spec);
        self.hooks.append(SessionRecord::Config {
            t: now_ms(),
            model: Some(spec.to_string()),
            effort: None,
        });
        Ok(())
    }

    /// Main's effort from the next call on (None = the model's default), and
    /// what a subagent starts on from now; recorded like the model. A model
    /// the catalog says does not take this effort is sent the lowest one it
    /// takes, and the [`Ui`](crate::Ui) is told once.
    pub fn set_effort(&mut self, effort: Option<Effort>) {
        self.agent.effort = effort;
        self.hooks.set_effort(effort);
        self.hooks.append(SessionRecord::Config {
            t: now_ms(),
            model: None,
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
    pub async fn clear(&mut self) -> anyhow::Result<()> {
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
