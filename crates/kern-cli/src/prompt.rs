//! The system prompt, built before every model call: instruction files are read
//! fresh (an edited AGENTS.md takes effect on the next call); skills are the
//! list found at start or at the last /skills.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kern::{
    SystemPrompt,
    context::{InstructionOpts, PromptInputs, build_system_prompt, find_instruction_files},
    fs::Fs,
    skills::Skill,
};

use crate::host::Host;

pub struct CliPrompt {
    pub fs: Arc<dyn Fs>,
    pub host: Arc<Host>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    pub instructions: InstructionOpts,
    /// Everything but the instructions, skills and model, fixed for the process.
    pub base: PromptInputs,
}

#[async_trait]
impl SystemPrompt for CliPrompt {
    async fn system_prompt(&self) -> String {
        let mut p = self.base.clone();
        p.model = self.host.model();
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
