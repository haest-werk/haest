//! The system prompt, built before every model call: instruction files are read
//! fresh (an edited AGENTS.md takes effect on the next call), the tool names are
//! the list this call advertises; skills are the list found at start or at the
//! last /skills, in the prompt only with `skill_catalog`. Main and every
//! subagent build it the same way, each naming its OWN model and tools (TS
//! `runtime.ts systemPrompt`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kern_settings::Settings;

use crate::{
    Host,
    kern::{
        SystemPrompt, Workspace,
        context::{
            InstructionOpts, PromptInputs, build_system_prompt, current_git_branch,
            find_instruction_files,
        },
        fs::Fs,
        now_ms,
        session::iso_utc,
        skills::{Skill, SkillOpts},
        tool::ToolSource,
    },
};

/// Which instruction files the `context` setting asks for.
pub fn instruction_opts(s: &Settings) -> InstructionOpts {
    InstructionOpts {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        config_dir: None,
        sources: s.context,
    }
}

/// Which skill dirs the `skills` setting asks for.
pub fn skill_opts(s: &Settings) -> SkillOpts {
    SkillOpts {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        config_dir: s.home.join(".claude"),
        sources: s.skills,
    }
}

pub struct RuntimePrompt {
    pub fs: Arc<dyn Fs>,
    /// Its settings say which instruction files to read (`context`) and
    /// whether the skill list goes in (`skill_catalog`).
    pub host: Arc<Host>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    /// Everything but the instructions, skills, tools and model, fixed for the process.
    pub base: PromptInputs,
}

impl RuntimePrompt {
    /// The parts fixed for the process; the model and tools are per agent.
    pub async fn new(
        host: &Arc<Host>,
        fs: &Arc<dyn Fs>,
        skills: &Arc<Mutex<Vec<Skill>>>,
        workspace: &dyn Workspace,
    ) -> Self {
        let s = &host.settings;
        let note = workspace.prompt_note();
        let base = PromptInputs {
            identity: s.system_prompt.clone(),
            cwd: s.cwd.to_string_lossy().into_owned(),
            platform: platform(),
            date: iso_utc(now_ms())[..10].to_string(),
            git_branch: current_git_branch(fs.as_ref(), &s.cwd).await,
            workspace_note: (!note.trim().is_empty()).then_some(note),
            append_system_prompt: s.append_system_prompt.clone(),
            ..Default::default()
        };
        Self {
            fs: fs.clone(),
            host: host.clone(),
            skills: skills.clone(),
            base,
        }
    }

    pub async fn build(&self, model: String, tool_names: Vec<String>) -> String {
        let s = &self.host.settings;
        let mut p = self.base.clone();
        p.model = model;
        p.tool_names = tool_names;
        p.instructions = find_instruction_files(self.fs.as_ref(), &instruction_opts(s)).await;
        if s.skill_catalog {
            p.skills = self.skills.lock().unwrap().clone();
        }
        build_system_prompt(&p)
    }
}

/// One agent's prompt: its model (None = the conversation's current one) and
/// the tools its next call advertises. Wrap it in the mods' `ModSystemPrompt`.
pub struct AgentPrompt {
    pub prompt: Arc<RuntimePrompt>,
    pub model: Option<String>,
    pub tools: Arc<dyn ToolSource>,
}

#[async_trait]
impl SystemPrompt for AgentPrompt {
    async fn system_prompt(&self) -> String {
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| self.prompt.host.model());
        let names = self
            .tools
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        self.prompt.build(model, names).await
    }
}

/// `macos` -> `darwin`: the name the TS kern (and most models' training data) uses.
pub fn platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".into(),
        os => os.into(),
    }
}
