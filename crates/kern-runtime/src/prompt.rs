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
        session::iso_utc,
        skills::{Skill, SkillOpts, SkillSources},
        tool::ToolSource,
    },
    now_ms,
};

/// Which instruction files the `context` setting asks for.
pub fn instruction_opts(s: &Settings) -> InstructionOpts {
    InstructionOpts {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        config_dir: None,
        agents_files: s.context.has("agents"),
        claude_files: s.context.has("claude"),
    }
}

/// Which skill dirs the `skills` setting asks for.
pub fn skill_opts(s: &Settings) -> SkillOpts {
    SkillOpts {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        config_dir: s.home.join(".claude"),
        sources: SkillSources {
            agents: s.skills.has("agents"),
            claude: s.skills.has("claude"),
            plugins: s.skills.has("plugins"),
        },
    }
}

pub struct RuntimePrompt {
    pub fs: Arc<dyn Fs>,
    pub host: Arc<Host>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    pub instructions: InstructionOpts,
    /// The skill list goes in the prompt (`skill_catalog`).
    pub catalog: bool,
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
            instructions: instruction_opts(s),
            catalog: s.skill_catalog,
            base,
        }
    }

    pub async fn build(&self, model: String, tool_names: Vec<String>) -> String {
        let mut p = self.base.clone();
        // The sandbox's limits only matter to a call that can touch it.
        let touches_files = ["Read", "Write", "Edit", "Bash", "Grep", "Glob"]
            .iter()
            .any(|t| tool_names.iter().any(|n| n == t));
        if !touches_files {
            p.workspace_note = None;
        }
        p.model = model;
        p.tool_names = tool_names;
        p.instructions = find_instruction_files(self.fs.as_ref(), &self.instructions).await;
        if self.catalog {
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
