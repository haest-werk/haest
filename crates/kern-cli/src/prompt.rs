//! The system prompt, built before every model call: instruction files are read
//! fresh (an edited AGENTS.md takes effect on the next call), the tool names are
//! the list this call advertises; skills are the list found at start or at the
//! last /skills. Main and every subagent build it the same way, each naming its
//! OWN model and tools (TS `runtime.ts systemPrompt`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kern::{
    SystemPrompt,
    context::{InstructionOpts, PromptInputs, build_system_prompt, find_instruction_files},
    fs::Fs,
    skills::Skill,
    tool::ToolSource,
};

use crate::host::Host;

pub struct CliPrompt {
    pub fs: Arc<dyn Fs>,
    pub host: Arc<Host>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    pub instructions: InstructionOpts,
    /// Everything but the instructions, skills, tools and model, fixed for the process.
    pub base: PromptInputs,
}

impl CliPrompt {
    pub async fn build(&self, model: String, tool_names: Vec<String>) -> String {
        let mut p = self.base.clone();
        p.model = model;
        p.tool_names = tool_names;
        p.instructions = find_instruction_files(self.fs.as_ref(), &self.instructions).await;
        p.skills = self.skills.lock().unwrap().clone();
        build_system_prompt(&p)
    }
}

/// One agent's prompt: its model (None = the conversation's current one) and
/// the tools its next call advertises. Wrap it in the mods' `ModSystemPrompt`.
pub struct AgentPrompt {
    pub prompt: Arc<CliPrompt>,
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
