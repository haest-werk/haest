//! Every setting as a field. [`Config`] holds the runtime's [`Settings`] and
//! the ones only a loader reads; [`set`] and [`get`] go by the setting's name,
//! in the one spelling [`crate::value::check`] gives a value, so what a file
//! says and what `kern settings` shows are the same text.

use std::{collections::BTreeMap, path::PathBuf};

use kern_core::{Effort, WorkspaceMode, deferred_tools::DeferMode, provider::WebSearchEngine};
use kern_settings::{BuiltinTools, ContextSources, Settings, SkillSources, WordSet};

use crate::value::{number, parse_bool, split_list, var, var_parts};

kern_core::word_enum! {
    /// What a process kern starts (bash, a background job, an MCP server) gets of
    /// kern's own environment. Codex's `shell_environment_policy.inherit`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum EnvPassthrough ("env_passthrough") {
        /// Nothing: an MCP server gets only its own `env`.
        None = "none",
        /// PATH, HOME, the user, the locale and the temp dirs ([`CORE_VARS`]).
        #[default]
        Core = "core",
        /// Every variable, API keys included (Claude Code's way).
        All = "all",
    }
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
    /// `env`: each variable's value as written; its `${NAME}`s are filled in
    /// when loaded ([`crate::Loaded::env`]).
    pub env: BTreeMap<String, String>,
    /// Start the project's `.mcp.json` servers (a trusted dir only).
    pub mcp: bool,
    /// OpenRouter's provider routing, the JSON as written: parsed into
    /// [`crate::Loaded::routing`].
    pub provider_routing: Option<String>,
}

impl Config {
    pub fn bare(cwd: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Settings::bare(cwd, home).into()
    }
}

/// These settings, and the loader's own bare.
impl From<Settings> for Config {
    fn from(settings: Settings) -> Self {
        Self {
            settings,
            profile: None,
            trusted_dirs: Vec::new(),
            env_passthrough: EnvPassthrough::Core,
            env: BTreeMap::new(),
            mcp: false,
            provider_routing: None,
        }
    }
}

/// A value in the checked spelling, and back. None = unset.
trait Raw: Sized {
    /// A count: a whole number up to this, never a fraction (`0.5` read as 0
    /// would turn a cap off). None = any number.
    const WHOLE_UP_TO: Option<f64> = None;
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
word!(
    Effort,
    WorkspaceMode,
    DeferMode,
    EnvPassthrough,
    WebSearchEngine
);

impl Raw for f64 {
    fn to_raw(&self) -> Option<String> {
        Some(self.to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        number(raw, None)
    }
}

macro_rules! count {
    ($($t:ty),+) => {$(
        impl Raw for $t {
            const WHOLE_UP_TO: Option<f64> = Some(<$t>::MAX as f64);
            fn to_raw(&self) -> Option<String> {
                Some(self.to_string())
            }
            fn from_raw(raw: &str) -> Result<Self, String> {
                number(raw, Self::WHOLE_UP_TO).map(|n| n as $t)
            }
        }
    )+};
}
count!(u32, u64, usize);

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
    const WHOLE_UP_TO: Option<f64> = T::WHOLE_UP_TO;
    fn to_raw(&self) -> Option<String> {
        self.as_ref().and_then(T::to_raw)
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        T::from_raw(raw).map(Some)
    }
}

type Get = fn(&Config) -> Option<String>;
type Set = fn(&mut Config, &str) -> Result<(), String>;

/// `web_search`: `off` is a value (no search), not unset, so it does not go
/// by the `Option` rule every other optional setting follows.
const WEB_SEARCH: (&str, Get, Set) = (
    "web_search",
    |c| {
        Some(
            c.settings
                .web_search
                .map_or("off", WebSearchEngine::as_str)
                .into(),
        )
    },
    |c, raw| {
        c.settings.web_search = match raw {
            "off" => None,
            engine => Some(engine.parse()?),
        };
        Ok(())
    },
);

/// `env`: a `NAME=VALUE` adds that variable to the ones set before it. It
/// reads back as the names only: a value may be a key, and `kern settings`
/// shows what [`get`] gives.
const ENV: (&str, Get, Set) = (
    "env",
    |c| (!c.env.is_empty()).then(|| c.env.keys().cloned().collect::<Vec<_>>().join(",")),
    |c, raw| {
        let entry = var(raw)?;
        let (name, value) = var_parts(&entry);
        c.env.insert(name.into(), value.into());
        Ok(())
    },
);

/// `key => the field it is`, read and written by its [`Raw`]; then the
/// settings that have their own get and set.
macro_rules! fields {
    ($($key:literal => $($path:ident).+),+ $(,)?; $($own:expr),* $(,)?) => {
        const FIELDS: &[(&str, Get, Set)] = &[$((
            $key,
            |c| Raw::to_raw(&c.$($path).+),
            |c, raw| {
                c.$($path).+ = Raw::from_raw(raw)?;
                Ok(())
            },
        ),)+ $($own),*];

        /// Whether `key`'s field is a count, and its largest value: what its
        /// type says ([`Raw::WHOLE_UP_TO`]), so the check and the field agree.
        pub(crate) fn whole_up_to(key: &str) -> Option<f64> {
            fn of<T: Raw>(_: fn(&Config) -> &T) -> Option<f64> {
                T::WHOLE_UP_TO
            }
            match key {
                $($key => of(|c| &c.$($path).+),)+
                _ => None,
            }
        }
    };
}

fields! {
    "profile" => profile,
    "model" => settings.model,
    "effort" => settings.effort,
    "system_prompt" => settings.system_prompt,
    "append_system_prompt" => settings.append_system_prompt,
    "provider_routing" => provider_routing,
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
    "debug" => settings.debug;
    WEB_SEARCH,
    ENV,
}

fn field(key: &str) -> Option<(Get, Set)> {
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
