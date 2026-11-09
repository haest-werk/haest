//! The system prompt, built before every model call: instruction files are read
//! fresh (an edited AGENTS.md takes effect on the next call), the tool names are
//! the list this call advertises; skills are the list found at start or at the
//! last /skills.

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
    pub tools: Arc<dyn ToolSource>,
    pub instructions: InstructionOpts,
    /// Everything but the instructions, skills, tools and model, fixed for the process.
    pub base: PromptInputs,
}

#[async_trait]
impl SystemPrompt for CliPrompt {
    async fn system_prompt(&self) -> String {
        let mut p = self.base.clone();
        p.model = self.host.model();
        p.tool_names = self
            .tools
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        p.instructions = find_instruction_files(self.fs.as_ref(), &self.instructions).await;
        p.skills = self.skills.lock().unwrap().clone();
        build_system_prompt(&p)
    }
}

/// `macos` -> `darwin`: the name the TS kern (and most models' training data) uses.
pub fn platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".into(),
        os => os.into(),
    }
}
