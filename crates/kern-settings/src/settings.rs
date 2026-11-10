//! The resolved values, typed: what a host builds a conversation from.

mod backends;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub use backends::ImageConfig;
use kern_core::{
    Effort, WorkspaceMode, deferred_tools::DeferMode, ledger::LedgerCaps, limits::TurnLimits,
    provider::WebSearchConfig,
};
use kern_tools::agents::SubagentCaps;

use crate::{
    resolve::{Resolved, absolute},
    value::{parse_bool, split_list},
};

/// Which of a list setting's words are on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picked(Vec<String>);

impl Picked {
    /// Words as a list setting holds them once checked ([`crate::value::check`]).
    pub fn from_words(words: Vec<String>) -> Self {
        Self(words)
    }

    pub fn has(&self, word: &str) -> bool {
        self.0.iter().any(|w| w == word)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn words(&self) -> &[String] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub model: String,
    pub effort: Option<Effort>,
    pub workspace: WorkspaceMode,
    /// Replaces kern's identity text.
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    /// Instruction files: `agents`, `claude`.
    pub context: Picked,
    /// Skill dirs: `agents`, `claude`, `plugins`.
    pub skills: Picked,
    pub skill_catalog: bool,
    /// Built-in tools by their setting word (`read`, `web_fetch`, ...).
    pub tools: Picked,
    /// None = no Code tool; Some = the extra names it may call.
    pub code: Option<Vec<String>>,
    pub agents: bool,
    /// None = off.
    pub web_search: Option<WebSearchConfig>,
    /// GenerateImage's backend; None = off (or on with no OpenRouter key: see `notes`).
    pub image: Option<ImageConfig>,
    pub mcp: bool,
    pub deferred_tools: DeferMode,
    pub mods: bool,
    pub project_mods: bool,
    pub approvals: bool,
    /// The working directory is inside a trusted dir: its .mcp.json and
    /// .kern/mods may run.
    pub trusted: bool,
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
    pub debug: bool,
    /// What the host should tell the human: a setting asked for and not honoured.
    pub notes: Vec<String>,
}

/// Reads one resolved value; every value was checked when it was read in.
struct Read<'a>(&'a Resolved);

impl Read<'_> {
    fn text(&self, key: &str) -> Option<String> {
        self.0.get(key).filter(|v| !v.is_empty()).map(String::from)
    }

    fn flag(&self, key: &str) -> bool {
        self.0.get(key).and_then(parse_bool).unwrap_or(false)
    }

    fn list(&self, key: &str) -> Vec<String> {
        self.0.get(key).map(split_list).unwrap_or_default()
    }

    fn num(&self, key: &str) -> f64 {
        self.0.get(key).and_then(|v| v.parse().ok()).unwrap_or(0.0)
    }

    fn opt_num(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(|v| v.parse().ok())
    }

    fn parsed<T: std::str::FromStr<Err = String>>(&self, key: &str) -> Result<Option<T>, String> {
        self.text(key).map(|v| v.parse()).transpose()
    }
}

fn env_var<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
}

impl Settings {
    /// `env` is read for what is not kern's to name: XDG dirs and the OpenRouter key.
    pub fn from(
        r: &Resolved,
        cwd: PathBuf,
        home: PathBuf,
        env: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let s = Read(r);
        let mut notes = Vec::new();
        let effort = s.parsed("effort")?;
        let workspace = s.parsed("workspace")?.unwrap_or(WorkspaceMode::Sandbox);
        let data_dir = s
            .text("data_dir")
            .map(|d| absolute(&d, &cwd, &home))
            .or_else(|| env_var(env, "XDG_DATA_HOME").map(|d| PathBuf::from(d).join("kern")))
            .unwrap_or_else(|| home.join(".local/share/kern"));
        let web_search = backends::web_search(&s)?;
        let image = match s.flag("image") {
            false => None,
            true => {
                let img = backends::image(&s, env);
                if img.is_none() {
                    notes.push(
                        "image is on, but OPENROUTER_API_KEY is not set: no GenerateImage.".into(),
                    );
                }
                img
            }
        };
        let d = SubagentCaps::default();
        Ok(Self {
            model: s
                .text("model")
                .unwrap_or_else(|| crate::registry::DEFAULT_MODEL.into()),
            effort,
            workspace,
            system_prompt: s.text("system_prompt"),
            append_system_prompt: s.text("append_system_prompt"),
            context: Picked(s.list("context")),
            skills: Picked(s.list("skills")),
            skill_catalog: s.flag("skill_catalog"),
            tools: Picked(s.list("tools")),
            code: s.flag("code").then(|| s.list("code_tools")),
            agents: s.flag("agents"),
            web_search,
            image,
            mcp: s.flag("mcp"),
            deferred_tools: s.parsed("deferred_tools")?.unwrap_or(DeferMode::Proxy),
            mods: s.flag("mods"),
            project_mods: s.flag("project_mods"),
            approvals: s.flag("approvals"),
            trusted: r.trusted,
            context_window: s
                .opt_num("context_window")
                .map(|n| n as u64)
                .filter(|n| *n > 1000),
            compact_at: s.num("compact_at") as u64,
            limits: TurnLimits {
                max_turn_usd: s.num("max_turn_usd"),
                max_turn_tokens: s.num("max_turn_tokens") as u64,
                max_request_tokens: s.num("max_request_tokens") as u64,
                max_turn_steps: s.num("max_turn_steps") as u32,
                max_turn_ms: (s.num("max_turn_minutes") * 60_000.0) as u64,
                max_repeats: s.num("max_repeats") as u32,
            },
            turn_tokens_set: r.is_set("max_turn_tokens"),
            request_tokens_set: r.is_set("max_request_tokens"),
            ledger: LedgerCaps {
                max_usd: s.num("max_conversation_usd"),
                max_unpriced_tokens: s.num("max_conversation_tokens") as u64,
            },
            max_agent_turns: s.num("max_agent_turns") as u32,
            subagents: SubagentCaps {
                max_depth: s
                    .opt_num("max_agent_depth")
                    .map_or(d.max_depth, |n| n as u32),
                max_concurrent: s
                    .opt_num("max_subagents")
                    .map_or(d.max_concurrent, |n| n as usize),
                usd: s.opt_num("subagent_usd").unwrap_or(d.usd),
                steps: s.opt_num("subagent_steps").map_or(d.steps, |n| n as u32),
                minutes: s
                    .opt_num("subagent_minutes")
                    .map_or(d.minutes, |n| n as u64),
            },
            data_dir,
            config_dir: config_dir(r.get("config_dir"), env, &cwd, &home),
            debug: s.flag("debug"),
            notes,
            cwd,
            home,
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

/// `<config dir>`: the setting (relative to `cwd`), else $XDG_CONFIG_HOME/kern,
/// else ~/.config/kern.
pub fn config_dir(
    set: Option<&str>,
    env: &HashMap<String, String>,
    cwd: &Path,
    home: &Path,
) -> PathBuf {
    set.map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|d| absolute(d, cwd, home))
        .or_else(|| env_var(env, "XDG_CONFIG_HOME").map(|d| PathBuf::from(d).join("kern")))
        .unwrap_or_else(|| home.join(".config/kern"))
}
