//! Every setting as a field. [`Config`] holds the runtime's [`Settings`] and
//! the four only a loader reads; [`set`] and [`get`] go by the setting's name,
//! in the one spelling [`crate::value::check`] gives a value, so what a file
//! says and what `kern settings` shows are the same text.

use std::path::PathBuf;

use kern_core::{Effort, WorkspaceMode, deferred_tools::DeferMode, provider::WebSearchEngine};
use kern_settings::{BuiltinTools, ContextSources, Settings, SkillSources, WordSet};

use crate::value::{parse_bool, split_list};

/// What a process kern starts (bash, a background job, an MCP server) gets of
/// kern's own environment. Codex's `shell_environment_policy.inherit`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EnvPassthrough {
    /// Nothing: an MCP server gets only its own `env`.
    None,
    /// PATH, HOME, the user, the locale and the temp dirs ([`CORE_VARS`]).
    #[default]
    Core,
    /// Every variable, API keys included (Claude Code's way).
    All,
}

/// `core`: Codex's list.
pub const CORE_VARS: &[&str] = &[
    "PATH", "SHELL", "TMPDIR", "TEMP", "TMP", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME",
    "USER",
];

/// Every setting's value.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// What the runtime is handed.
    pub settings: Settings,
    pub profile: Option<String>,
    /// As written: made absolute when trust is decided.
    pub trusted_dirs: Vec<String>,
    pub env_passthrough: EnvPassthrough,
    /// Start the project's `.mcp.json` servers (a trusted dir only).
    pub mcp: bool,
}

impl Config {
    pub fn bare(cwd: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            settings: Settings::bare(cwd, home),
            profile: None,
            trusted_dirs: Vec::new(),
            env_passthrough: EnvPassthrough::Core,
            mcp: false,
        }
    }
}

/// A value in the checked spelling, and back. None = unset.
trait Raw: Sized {
    fn to_raw(&self) -> Option<String>;
    fn from_raw(raw: &str) -> Result<Self, String>;
}

/// A type that already reads and writes itself as one word.
macro_rules! word {
    ($($t:ty),+) => {$(
        impl Raw for $t {
            fn to_raw(&self) -> Option<String> {
                Some(self.as_str().into())
            }
            fn from_raw(raw: &str) -> Result<Self, String> {
                raw.parse()
            }
        }
    )+};
}
word!(Effort, WorkspaceMode, DeferMode);

macro_rules! number {
    ($($t:ty),+) => {$(
        impl Raw for $t {
            fn to_raw(&self) -> Option<String> {
                Some(self.to_string())
            }
            fn from_raw(raw: &str) -> Result<Self, String> {
                raw.parse::<f64>().map(|n| n as $t).map_err(|e| e.to_string())
            }
        }
    )+};
}
number!(f64, u32, u64, usize);

macro_rules! words {
    ($($t:ty),+) => {$(
        impl Raw for $t {
            fn to_raw(&self) -> Option<String> {
                Some(self.words().join(","))
            }
            fn from_raw(raw: &str) -> Result<Self, String> {
                let words = split_list(raw);
                <$t>::from_words(words.iter().map(String::as_str))
                    .map_err(|w| format!("\"{w}\" is not one of {}", <$t>::WORDS.join(", ")))
            }
        }
    )+};
}
words!(BuiltinTools, ContextSources, SkillSources);

impl Raw for bool {
    fn to_raw(&self) -> Option<String> {
        Some(self.to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        parse_bool(raw).ok_or_else(|| format!("\"{raw}\" must be true or false"))
    }
}

impl Raw for String {
    fn to_raw(&self) -> Option<String> {
        Some(self.clone())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        Ok(raw.into())
    }
}

impl Raw for PathBuf {
    fn to_raw(&self) -> Option<String> {
        Some(self.display().to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        Ok(raw.into())
    }
}

impl Raw for Vec<String> {
    fn to_raw(&self) -> Option<String> {
        Some(self.join(","))
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        Ok(split_list(raw))
    }
}

impl<T: Raw> Raw for Option<T> {
    fn to_raw(&self) -> Option<String> {
        self.as_ref().and_then(T::to_raw)
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        T::from_raw(raw).map(Some)
    }
}

/// `off` is None.
struct Engine(Option<WebSearchEngine>);

impl Raw for Engine {
    fn to_raw(&self) -> Option<String> {
        Some(self.0.map_or("off", WebSearchEngine::as_str).into())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        match raw {
            "off" => Ok(Self(None)),
            engine => engine.parse().map(|e| Self(Some(e))),
        }
    }
}

impl Raw for EnvPassthrough {
    fn to_raw(&self) -> Option<String> {
        let word = match self {
            Self::None => "none",
            Self::Core => "core",
            Self::All => "all",
        };
        Some(word.into())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        match raw {
            "none" => Ok(Self::None),
            "core" => Ok(Self::Core),
            "all" => Ok(Self::All),
            other => Err(format!("\"{other}\" must be none, core or all")),
        }
    }
}

type Get = fn(&Config) -> Option<String>;
type Set = fn(&mut Config, &str) -> Result<(), String>;

/// `key => the field it is`.
macro_rules! fields {
    ($($key:literal => $($path:ident).+),+ $(,)?) => {
        const FIELDS: &[(&str, Get, Set)] = &[$((
            $key,
            |c| Raw::to_raw(&c.$($path).+),
            |c, raw| {
                c.$($path).+ = Raw::from_raw(raw)?;
                Ok(())
            },
        )),+];
    };
}

fields! {
    "profile" => profile,
    "model" => settings.model,
    "effort" => settings.effort,
    "system_prompt" => settings.system_prompt,
    "append_system_prompt" => settings.append_system_prompt,
    "context" => settings.context,
    "skills" => settings.skills,
    "skill_catalog" => settings.skill_catalog,
    "tools" => settings.tools,
    "code" => settings.code,
    "code_tools" => settings.code_tools,
    "agents" => settings.agents,
    "image" => settings.image,
    "image_model" => settings.image_model,
    "image_models" => settings.image_models,
    "mcp" => mcp,
    "deferred_tools" => settings.deferred_tools,
    "workspace" => settings.workspace,
    "env_passthrough" => env_passthrough,
    "mods" => settings.mods,
    "project_mods" => settings.project_mods,
    "approvals" => settings.approvals,
    "trusted_dirs" => trusted_dirs,
    "max_turn_usd" => settings.limits.max_turn_usd,
    "max_turn_tokens" => settings.limits.max_turn_tokens,
    "max_request_tokens" => settings.limits.max_request_tokens,
    "max_turn_steps" => settings.limits.max_turn_steps,
    "max_turn_minutes" => settings.limits.max_turn_minutes,
    "max_repeats" => settings.limits.max_repeats,
    "max_conversation_usd" => settings.limits.max_conversation_usd,
    "max_conversation_tokens" => settings.limits.max_conversation_tokens,
    "context_window" => settings.limits.context_window,
    "compact_at" => settings.limits.compact_at,
    "max_agent_turns" => settings.max_agent_turns,
    "max_agent_depth" => settings.subagents.max_depth,
    "max_subagents" => settings.subagents.max_concurrent,
    "subagent_usd" => settings.subagents.usd,
    "subagent_steps" => settings.subagents.steps,
    "subagent_minutes" => settings.subagents.minutes,
    "data_dir" => settings.data_dir,
    "config_dir" => settings.config_dir,
    "debug" => settings.debug,
}

/// `web_search` is the one setting whose "off" is a value, not unset.
fn web_search(c: &Config) -> Option<String> {
    Engine(c.settings.web_search).to_raw()
}

fn set_web_search(c: &mut Config, raw: &str) -> Result<(), String> {
    c.settings.web_search = Engine::from_raw(raw)?.0;
    Ok(())
}

fn field(key: &str) -> Option<(Get, Set)> {
    if key == "web_search" {
        return Some((web_search, set_web_search));
    }
    FIELDS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, g, s)| (*g, *s))
}

/// Set `key` from its checked value.
pub fn set(c: &mut Config, key: &str, raw: &str) -> Result<(), String> {
    let (_, set) = field(key).ok_or_else(|| format!("unknown setting \"{key}\""))?;
    set(c, raw)
}

/// `key`'s value in the checked spelling; None = unset (or no such setting).
pub fn get(c: &Config, key: &str) -> Option<String> {
    field(key).and_then(|(get, _)| get(c))
}
