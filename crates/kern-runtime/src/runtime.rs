//! A built conversation, and what a host changes on it while it runs: the
//! model, the effort, a fresh session (/clear), the skill list. Each change
//! goes to the agent, the host and the session file together; the host's own
//! setters are crate-private, so these are the only way to make one.

use std::sync::{Arc, Mutex};

use anyhow::anyhow;

use crate::{
    Host,
    inbox::InboxQueue,
    kern::{
        Agent, Effort,
        fs::Fs,
        mcp::McpHub,
        mods::Mods,
        now_ms,
        session::SessionRecord,
        skills::{Skill, SkillScanRoots, skills_in},
        task_board::TaskBoard,
        tools::{JobHub, agents::SubagentHub},
    },
    store::{new_session, record_start},
};

pub struct Runtime<U: ?Sized> {
    pub agent: Agent,
    pub host: Arc<Host>,
    /// The host's own UI, as it handed it in.
    pub ui: Arc<U>,
    pub fs: Arc<dyn Fs>,
    pub board: Arc<TaskBoard>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    /// Where the skills were found at the start; the sandbox mounts these.
    pub(crate) skill_roots: SkillScanRoots,
    pub mcp: Arc<McpHub>,
    pub mods: Arc<Mods>,
    /// Turns the human did not start, waiting for main.
    pub inbox: InboxQueue,
    /// Main's subagents (none run when `agents` is off).
    pub agents: Arc<SubagentHub>,
    /// Background shell jobs; None unless Bash runs in the real workspace.
    pub jobs: Option<Arc<JobHub>>,
}

impl<U: ?Sized> Runtime<U> {
    /// The session file records go to now; /clear changes it.
    pub fn session_id(&self) -> String {
        self.host.store().session_id().to_string()
    }

    /// Main's model from the next call on; a spec that does not resolve
    /// changes nothing. The session file records it, so a resume keeps it.
    pub fn set_model(&mut self, spec: &str) -> anyhow::Result<()> {
        self.agent
            .deps()
            .models
            .resolve(spec)
            .map_err(|e| anyhow!(e))?;
        self.agent.model_spec = spec.to_string();
        self.host.set_model(spec);
        self.host.append(SessionRecord::Config {
            t: now_ms(),
            model: Some(spec.to_string()),
            effort: None,
        });
        Ok(())
    }

    /// Main's effort from the next call on (None = the model's default);
    /// recorded like the model.
    pub fn set_effort(&mut self, effort: Option<Effort>) {
        self.agent.effort = effort;
        self.host.set_effort(effort);
        self.host.append(SessionRecord::Config {
            t: now_ms(),
            model: None,
            effort: Some(effort),
        });
    }

    /// A fresh session: new id, new file, empty history and task list; the
    /// ledger carries over. The new file is on disk, header first, before
    /// anything switches to it: if it cannot be created, nothing changes.
    pub async fn clear(&mut self) -> anyhow::Result<()> {
        let store = new_session(&self.fs, &self.host.settings, self.host.ui.clone()).await?;
        self.host.swap_store(Arc::new(store));
        self.agent.replace_history(Vec::new());
        self.board.restore(Vec::new());
        record_start(&self.host, &self.agent);
        Ok(())
    }

    /// The skill dirs found at the start, read again: a skill added to one
    /// is offered from now on. A plugin installed since the start is not (its
    /// dir is not in the sandbox); a new conversation picks it up.
    pub async fn rescan_skills(&self) -> Vec<Skill> {
        let found = skills_in(self.fs.as_ref(), &self.skill_roots).await;
        *self.skills.lock().unwrap() = found.clone();
        found
    }
}
