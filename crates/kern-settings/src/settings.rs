//! The values a conversation is built from, typed. A host writes them out
//! (`Settings { tools: BuiltinTools::ALL, ..Settings::bare(cwd, home) }`) or
//! has a loader fill them in (kern-config: files, `KERN_*` variables, flags).

use std::path::PathBuf;

use kern_core::{
    Effort, WorkspaceMode,
    deferred_tools::DeferMode,
    ledger::LedgerCaps,
    limits::TurnLimits,
    provider::{WebSearchConfig, WebSearchEngine},
    skills::SkillSources,
};
use kern_tools::{
    agents::SubagentCaps,
    image::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS},
};

use crate::sets::{BuiltinTools, ContextSources};

/// The model when none is configured.
pub const DEFAULT_MODEL: &str = "openrouter:anthropic/claude-sonnet-5.5";

/// The caps. 0 = no cap, everywhere.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// One turn's spend.
    pub max_turn_usd: f64,
    /// One turn's tokens; None = 10x the model's window.
    pub max_turn_tokens: Option<u64>,
    /// One model call's input tokens; None = 90% of the window.
    pub max_request_tokens: Option<u64>,
    /// Model calls in one turn.
    pub max_turn_steps: u32,
    /// One turn's wall clock.
    pub max_turn_minutes: f64,
    /// The same tool call in a row before the guard stops it.
    pub max_repeats: u32,
    /// The whole conversation's spend.
    pub max_conversation_usd: f64,
    /// Tokens of models with no price, across the conversation.
    pub max_conversation_tokens: u64,
    /// The model's window; None = the catalog's, else 200K.
    pub context_window: Option<u64>,
    /// Compact past this many tokens; 0 = at 80% of the window only.
    pub compact_at: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Where kern works: relative paths, the instruction files and skills up from it.
    pub cwd: PathBuf,
    /// Where `~/.agents`, `~/.claude` and the default dirs are.
    pub home: PathBuf,
    /// `provider:model`.
    pub model: String,
    /// None = the model's own default.
    pub effort: Option<Effort>,
    /// Replaces kern's identity text.
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub context: ContextSources,
    pub skills: SkillSources,
    /// The skill list goes in the system prompt.
    pub skill_catalog: bool,
    pub tools: BuiltinTools,
    /// The Code tool: a JavaScript run that calls the other tools.
    pub code: bool,
    /// Extra tool names Code may call (exact, or `prefix*`).
    pub code_tools: Vec<String>,
    /// Agent, SendMessage and StopAgent.
    pub agents: bool,
    /// The provider's web search; None = off.
    pub web_search: Option<WebSearchEngine>,
    /// GenerateImage, over OpenRouter with the host's key.
    pub image: bool,
    pub image_model: String,
    /// The other models a call may pick; None = kern-tools' list.
    pub image_models: Option<Vec<String>>,
    /// How MCP tools reach the model.
    pub deferred_tools: DeferMode,
    pub workspace: WorkspaceMode,
    /// The human's mods in `<config_dir>/mods`.
    pub mods: bool,
    /// The project's mods in `.kern/mods`: the repo's code, runs as asked.
    pub project_mods: bool,
    /// The built-in approval mod: asks before a tool changes anything.
    pub approvals: bool,
    pub limits: Limits,
    /// Turns in a row a job or an agent starts with no human message; 0 = no cap.
    pub max_agent_turns: u32,
    pub subagents: SubagentCaps,
    /// Sessions, history and spill files.
    pub data_dir: PathBuf,
    /// The human's mods dir is `<config_dir>/mods`.
    pub config_dir: PathBuf,
    /// Log what kern does under the hood.
    pub debug: bool,
}

impl Settings {
    /// Bare kern: every capability off until asked for, every brake on.
    pub fn bare(cwd: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            cwd: cwd.into(),
            model: DEFAULT_MODEL.into(),
            effort: None,
            system_prompt: None,
            append_system_prompt: None,
            context: ContextSources::NONE,
            skills: SkillSources::default(),
            skill_catalog: false,
            tools: BuiltinTools::NONE,
            code: false,
            code_tools: Vec::new(),
            agents: false,
            web_search: None,
            image: false,
            image_model: DEFAULT_IMAGE_MODEL.into(),
            image_models: None,
            deferred_tools: DeferMode::Proxy,
            workspace: WorkspaceMode::Sandbox,
            mods: false,
            project_mods: false,
            approvals: true,
            limits: Limits {
                max_turn_usd: 2.0,
                max_turn_tokens: None,
                max_request_tokens: None,
                max_turn_steps: 100,
                max_turn_minutes: 60.0,
                max_repeats: 6,
                max_conversation_usd: 20.0,
                max_conversation_tokens: 20_000_000,
                context_window: None,
                compact_at: 400_000,
            },
            max_agent_turns: 30,
            subagents: SubagentCaps::default(),
            data_dir: home.join(".local/share/kern"),
            config_dir: home.join(".config/kern"),
            debug: false,
            home,
        }
    }

    /// The model's window: the override, else the catalog's, else 200K.
    pub fn window(&self, catalog_window: Option<u64>) -> u64 {
        let set = self.limits.context_window.filter(|n| *n > 1000);
        set.or(catalog_window).unwrap_or(200_000)
    }

    /// The turn caps for a model with this window: an unset token cap follows it.
    pub fn limits_for(&self, window: u64) -> TurnLimits {
        let l = &self.limits;
        TurnLimits {
            max_turn_usd: l.max_turn_usd,
            max_turn_tokens: l.max_turn_tokens.unwrap_or(window * 10),
            max_request_tokens: l.max_request_tokens.unwrap_or(window * 9 / 10),
            max_turn_steps: l.max_turn_steps,
            max_turn_ms: (l.max_turn_minutes * 60_000.0) as u64,
            max_repeats: l.max_repeats,
        }
    }

    /// The conversation's caps.
    pub fn ledger(&self) -> LedgerCaps {
        LedgerCaps {
            max_usd: self.limits.max_conversation_usd,
            max_unpriced_tokens: self.limits.max_conversation_tokens,
        }
    }

    /// None = no Code tool; Some = the extra names it may call.
    pub fn code_tools(&self) -> Option<Vec<String>> {
        self.code.then(|| self.code_tools.clone())
    }

    pub fn web_search_config(&self) -> Option<WebSearchConfig> {
        self.web_search.map(WebSearchConfig::new)
    }

    /// Every model GenerateImage may pick: `image_model` first, once.
    pub fn image_models(&self) -> Vec<String> {
        let others = match &self.image_models {
            Some(list) => list.clone(),
            None => DEFAULT_IMAGE_MODELS.map(String::from).to_vec(),
        };
        let model = &self.image_model;
        std::iter::once(model.clone())
            .chain(others.into_iter().filter(|m| m != model))
            .collect()
    }

    pub fn spill_dir(&self, session_id: &str) -> PathBuf {
        self.data_dir.join("spill").join(session_id)
    }
}
