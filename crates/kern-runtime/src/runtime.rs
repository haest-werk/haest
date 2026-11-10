//! A built conversation, and what a host changes on it while it runs: the
//! model, the effort, a fresh session (/clear), the skill list. Each change
//! goes to the agent, the host and the session file together.

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
        skills::{Skill, SkillOpts, discover_skills},
        task_board::TaskBoard,
        tools::{JobHub, agents::SubagentHub},
    },
    sessions::new_session_id,
    store::{init_store, open_store},
};

pub struct Runtime<U: ?Sized> {
    pub agent: Agent,
    pub host: Arc<Host>,
    /// The host's own UI, as it handed it in.
    pub ui: Arc<U>,
    pub fs: Arc<dyn Fs>,
    pub board: Arc<TaskBoard>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    pub skill_opts: SkillOpts,
    pub mcp: Arc<McpHub>,
    pub mods: Arc<Mods>,
    pub session_id: String,
    /// Turns the human did not start, waiting for main.
    pub inbox: InboxQueue,
    /// Main's subagents (none run when `agents` is off).
    pub agents: Arc<SubagentHub>,
    /// Background shell jobs; None unless Bash runs in the real workspace.
    pub jobs: Option<Arc<JobHub>>,
}

impl<U: ?Sized> Runtime<U> {
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

    /// A fresh session: new id, new file, empty history and task list. The
    /// ledger carries over. If the new file cannot be opened, nothing changes.
    pub fn clear(&mut self) -> anyhow::Result<()> {
        let id = new_session_id();
        let store = open_store(
            &self.fs,
            &self.host.settings.data_dir,
            &id,
            self.host.ui.clone(),
        )?;
        self.host.swap_store(Arc::new(store));
        self.agent.replace_history(Vec::new());
        init_store(&self.host, &self.agent, &id);
        self.board.restore(Vec::new());
        self.session_id = id;
        Ok(())
    }

    /// The skill dirs read again; the list the Skill tool and the prompt
    /// offer from now on.
    pub async fn rescan_skills(&self) -> Vec<Skill> {
        let found = discover_skills(self.fs.as_ref(), &self.skill_opts).await;
        *self.skills.lock().unwrap() = found.clone();
        found
    }
}
