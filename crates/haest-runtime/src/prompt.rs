//! The system prompt, built before every model call: instruction files are read
//! fresh (an edited AGENTS.md takes effect on the next call), the tool names are
//! the list this call advertises; skills are the list found at start or at the
//! last /skills, in the prompt only with `skill_catalog`. Main and every
//! subagent build it the same way, each naming its OWN model and tools (TS
//! `runtime.ts systemPrompt`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_settings::Settings;

use crate::{
    RuntimeHooks,
    haest::{
        SystemPrompt, Workspace,
        core::context::{
            InstructionOpts, PromptInputs, build_system_prompt, current_git_branch,
            find_instruction_files,
        },
        core::fs::Fs,
        core::session::iso_utc,
        core::skills::{Skill, SkillOpts},
        core::tool::ToolSource,
        now_ms,
    },
};

/// Which instruction files the `context` setting asks for (AGENTS.md,
/// CLAUDE.md and their kin), looked for from `cwd` and `home`: what
/// `find_instruction_files` takes.
pub fn instruction_opts(s: &Settings) -> InstructionOpts {
    InstructionOpts {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        config_dir: None,
        sources: s.context,
    }
}

/// Which skill dirs the `skills` setting asks for, under `cwd` and `home`,
/// with `~/.claude` as Claude Code's user dir (its skills and its plugin
/// list): what `skill_scan_roots` takes.
pub fn skill_opts(s: &Settings) -> SkillOpts {
    SkillOpts {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        config_dir: s.home.join(".claude"),
        sources: s.skills,
    }
}

/// The system prompt's parts that every agent of the conversation shares, and
/// how to build one for a given model and tool list ([`build`](Self::build)).
///
/// Wrap it in an [`AgentPrompt`] to give an agent its prompt.
pub struct RuntimePrompt {
    /// Where the instruction files are read from.
    pub fs: Arc<dyn Fs>,
    /// Its settings say which instruction files to read (`context`) and
    /// whether the skill list goes in (`skill_catalog`).
    pub hooks: Arc<RuntimeHooks>,
    /// The skills found; listed in the prompt with `skill_catalog` on.
    pub skills: Arc<Mutex<Vec<Skill>>>,
    /// Everything but the instructions, skills, tools and model, fixed for the process.
    pub base: PromptInputs,
}

impl RuntimePrompt {
    /// The parts fixed for the process; the model and tools are per agent.
    ///
    /// Reads them once, now: the identity text (`system_prompt`), the working
    /// dir, the platform, today's date, the git branch, the workspace's own
    /// note, `append_system_prompt`.
    pub async fn new(
        hooks: &Arc<RuntimeHooks>,
        fs: &Arc<dyn Fs>,
        skills: &Arc<Mutex<Vec<Skill>>>,
        workspace: &dyn Workspace,
    ) -> Self {
        let s = &hooks.settings;
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
            hooks: hooks.clone(),
            skills: skills.clone(),
            base,
        }
    }

    /// The system prompt for an agent on `model` offered `tool_names`: the
    /// fixed parts, the instruction files read again now, and the skill list
    /// with `skill_catalog` on.
    pub async fn build(&self, model: String, tool_names: Vec<String>) -> String {
        let s = &self.hooks.settings;
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
///
/// It is a [`SystemPrompt`]: the loop asks it before every model call.
pub struct AgentPrompt {
    /// The parts every agent shares.
    pub prompt: Arc<RuntimePrompt>,
    /// The model the prompt names; None = the conversation's current one
    /// ([`RuntimeHooks::model`]), as for main.
    pub model: Option<String>,
    /// The agent's tools: the prompt names the ones its next call offers.
    pub tools: Arc<dyn ToolSource>,
}

#[async_trait]
impl SystemPrompt for AgentPrompt {
    async fn system_prompt(&self) -> String {
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| self.prompt.hooks.model());
        let names = self
            .tools
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        self.prompt.build(model, names).await
    }
}

/// The platform the system prompt names: Rust's `std::env::consts::OS`, with
/// `macos` as `darwin`, the name the TS haest (and most models' training data)
/// uses.
pub fn platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".into(),
        os => os.into(),
    }
}
