//! Configuration: flags, then `KERN_*` variables, then defaults. Pure: the
//! environment comes in as a map, so every rule here is testable.
//!
//! The variable names and defaults are the TypeScript kern's (`host/config.ts`,
//! `host/limits-config.ts`); only the data dir differs (an XDG path, since this
//! kern runs on its own).

use std::{collections::HashMap, path::PathBuf};

use kern::{
    Effort, WorkspaceMode, deferred_tools::DeferMode, ledger::LedgerCaps, limits::TurnLimits,
    tools::agents::SubagentCaps,
};

use crate::args::Args;

mod agents;
mod features;

pub use features::{Features, ImageConfig};

pub type Env = HashMap<String, String>;

/// The model when none is configured.
pub const DEFAULT_MODEL: &str = "openrouter:anthropic/claude-sonnet-5.5";

/// A 1M window compacted at 80% re-sends ~800K tokens every step; 400K keeps a
/// long turn affordable.
const DEFAULT_COMPACT_AT: u64 = 400_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub model: String,
    pub effort: Option<Effort>,
    pub workspace: WorkspaceMode,
    /// Some("") = the latest session here; Some(id) = that one.
    pub resume: Option<String>,
    pub append_system_prompt: Option<String>,
    /// None = the catalog's window for the current model (200K for one it does not know).
    pub context_window: Option<u64>,
    /// Auto-compaction ceiling in tokens; 0 = 80% of the window only.
    pub compact_at: u64,
    /// The turn caps; the two token caps at 0 here follow the model's window (see `limits_for`).
    pub limits: TurnLimits,
    pub turn_tokens_set: bool,
    pub request_tokens_set: bool,
    pub ledger: LedgerCaps,
    /// Turns in a row a job or an agent may start with no human message; 0 = no cap.
    pub max_agent_turns: u32,
    pub subagents: SubagentCaps,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    /// How MCP tools reach the model [KERN_DEFERRED_TOOLS]: proxy (default), grow, off.
    pub deferred_tools: DeferMode,
    pub features: Features,
    pub debug: bool,
}

fn get<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
    env.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// A non-negative number; 0 = no cap. Garbage is refused, never silently uncapped.
fn cap(env: &Env, key: &str, fallback: f64) -> Result<f64, String> {
    match get(env, key) {
        None => Ok(fallback),
        Some(raw) => raw
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| format!("{key} \"{raw}\" must be a number >= 0 (0 = no cap)")),
    }
}

impl Config {
    pub fn from(args: &Args, env: &Env, cwd: PathBuf) -> Result<Self, String> {
        let home = get(env, "HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let effort = match args.effort.as_deref().or(get(env, "KERN_EFFORT")) {
            None => None,
            Some(raw) => Some(raw.parse::<Effort>()?),
        };
        let workspace = match args.workspace.as_deref().or(get(env, "KERN_WORKSPACE")) {
            None => WorkspaceMode::Sandbox,
            Some(raw) => match raw.parse::<WorkspaceMode>()? {
                WorkspaceMode::Shut => {
                    return Err(
                        "the CLI runs in a sandbox or the real workspace, not a shut one".into(),
                    );
                }
                m => m,
            },
        };
        let data_dir = get(env, "KERN_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| get(env, "XDG_DATA_HOME").map(|d| PathBuf::from(d).join("kern")))
            .unwrap_or_else(|| home.join(".local/share/kern"));
        let config_dir = get(env, "KERN_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config/kern"));
        let context_window = get(env, "KERN_CONTEXT_WINDOW")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 1000);
        let limits = TurnLimits {
            max_turn_usd: cap(env, "KERN_MAX_TURN_USD", 2.0)?,
            max_turn_tokens: cap(env, "KERN_MAX_TURN_TOKENS", 0.0)? as u64,
            max_request_tokens: cap(env, "KERN_MAX_REQUEST_TOKENS", 0.0)? as u64,
            max_turn_steps: cap(env, "KERN_MAX_TURN_STEPS", 100.0)? as u32,
            max_turn_ms: (cap(env, "KERN_MAX_TURN_MINUTES", 60.0)? * 60_000.0) as u64,
            max_repeats: cap(env, "KERN_MAX_REPEATS", 6.0)? as u32,
        };
        let mut features = Features::from(env)?;
        features.claude_context_files |= args.use_claude_context_files;
        Ok(Self {
            cwd: args.cwd.clone().unwrap_or(cwd),
            home,
            model: args
                .model
                .clone()
                .or_else(|| get(env, "KERN_MODEL").map(String::from))
                .unwrap_or_else(|| DEFAULT_MODEL.into()),
            effort,
            workspace,
            resume: args.resume.clone(),
            append_system_prompt: args.append_system_prompt.clone(),
            context_window,
            compact_at: cap(env, "KERN_COMPACT_AT", DEFAULT_COMPACT_AT as f64)? as u64,
            limits,
            turn_tokens_set: get(env, "KERN_MAX_TURN_TOKENS").is_some(),
            request_tokens_set: get(env, "KERN_MAX_REQUEST_TOKENS").is_some(),
            ledger: LedgerCaps {
                max_usd: cap(env, "KERN_MAX_CONVERSATION_USD", 20.0)?,
                max_unpriced_tokens: cap(env, "KERN_MAX_CONVERSATION_TOKENS", 20_000_000.0)? as u64,
            },
            max_agent_turns: agents::max_agent_turns(env)?,
            subagents: agents::subagent_caps(env)?,
            data_dir,
            config_dir,
            deferred_tools: match get(env, "KERN_DEFERRED_TOOLS") {
                None => DeferMode::Proxy,
                Some(raw) => raw.parse()?,
            },
            features,
            debug: get(env, "KERN_DEBUG").is_some(),
        })
    }

    /// The model's window: the override, else the catalog's, else 200K.
    pub fn window(&self, catalog_window: Option<u64>) -> u64 {
        self.context_window.or(catalog_window).unwrap_or(200_000)
    }

    /// The turn caps for a model with this window: an unset token cap follows it
    /// (request: 90% of the window; turn: 10x the window).
    pub fn limits_for(&self, window: u64) -> TurnLimits {
        let mut l = self.limits.clone();
        if !self.request_tokens_set {
            l.max_request_tokens = window * 9 / 10;
        }
        if !self.turn_tokens_set {
            l.max_turn_tokens = window * 10;
        }
        l
    }

    pub fn spill_dir(&self, session_id: &str) -> PathBuf {
        self.data_dir.join("spill").join(session_id)
    }
}

#[cfg(test)]
mod tests;
