//! Every setting as a field. [`Config`] holds the runtime's [`Settings`] and
//! the ones only a loader reads; [`Config::set`] and [`Config::get`] go by the
//! setting's name, in the one spelling [`crate::value::check`] gives a value,
//! so what a file says and what `haest settings` shows are the same text.

mod raw;

use std::{collections::BTreeMap, path::PathBuf};

use haest_core::provider::WebSearchEngine;
use haest_settings::Settings;

use self::raw::Raw;
use crate::{
    SetError,
    value::{var, var_parts},
};

haest_core::word_enum! {
    /// What a process haest starts (bash, a background job, an MCP server) gets of
    /// haest's own environment: the `env_passthrough` setting. Codex's
    /// `shell_environment_policy.inherit`. What the `env` setting passes on
    /// purpose comes on top, whichever it is.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum EnvPassthrough ("env_passthrough") {
        /// Nothing: a process gets only what `env` passes, and an MCP server
        /// its own `env` too.
        None = "none",
        /// PATH, HOME, the user, the locale and the temp dirs ([`CORE_VARS`]).
        #[default]
        Core = "core",
        /// Every variable, API keys included (Claude Code's way).
        All = "all",
    }
}

/// The variables `env_passthrough = core` passes on, when haest has them:
/// `PATH`, the shell, the temp dirs, `HOME`, the locale and the user. Codex's list.
pub const CORE_VARS: &[&str] = &[
    "PATH", "SHELL", "TMPDIR", "TEMP", "TMP", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME",
    "USER",
];

/// Every setting's value: the runtime's [`Settings`], and the settings only
/// the loader reads.
///
/// [`get`](Self::get) and [`set`](Self::set) reach any of them by the
/// setting's name.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// What the runtime is handed.
    pub settings: Settings,
    /// The profile `profile` names, if any; its values are already laid in.
    pub profile: Option<String>,
    /// As written: made absolute when trust is decided.
    pub trusted_dirs: Vec<String>,
    /// What a process haest starts gets of haest's environment; applied in
    /// [`crate::Loaded::child_env`].
    pub env_passthrough: EnvPassthrough,
    /// `env`: each variable's value as written; its `${NAME}`s are filled in
    /// when loaded ([`crate::Loaded::passed_env`]).
    pub env: BTreeMap<String, String>,
    /// Start the project's `.mcp.json` servers (a trusted dir only).
    pub mcp: bool,
    /// OpenRouter's provider routing, the JSON as written: parsed into
    /// [`crate::Loaded::routing`].
    pub provider_routing: Option<String>,
}

impl Config {
    /// Bare haest working in `cwd` with `home` as the home dir
    /// ([`Settings::bare`]), and the loader's own settings at their defaults:
    /// no profile, no trusted dirs, `env_passthrough = core`, no `env`, `mcp`
    /// off, no routing.
    pub fn bare(cwd: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Settings::bare(cwd, home).into()
    }

    /// Set the setting named `key` from `raw`, its value in the checked
    /// spelling: `true` / `false`, a number, a word, a comma list of words in
    /// any order (empty for none), text; for `env`, one `NAME=VALUE`, added to
    /// the ones there.
    ///
    /// This is how the loader writes a value it has checked. It does not check
    /// where the value may come from ([`Scope`](crate::Scope)), nor take every
    /// spelling a layer may use (`all` or `none` for a list, a word in another
    /// case).
    ///
    /// # Errors
    ///
    /// [`SetError::Unknown`] when there is no setting `key`,
    /// [`SetError::Value`] when `raw` is not a value it takes. Either way
    /// nothing changes.
    ///
    /// # Examples
    ///
    /// ```
    /// use haest_config::{Config, SetError};
    ///
    /// let mut c = Config::bare("/work", "/home/me");
    /// c.set("tools", "grep,read").unwrap();
    /// c.set("max_turn_usd", "5").unwrap();
    /// assert_eq!(c.get("tools").as_deref(), Some("read,grep"));
    /// assert_eq!(c.settings.caps.max_turn_usd, 5.0);
    ///
    /// assert!(matches!(c.set("tool", "read"), Err(SetError::Unknown { .. })));
    /// assert!(matches!(c.set("max_turn_steps", "2.5"), Err(SetError::Value { .. })));
    /// ```
    pub fn set(&mut self, key: &str, raw: &str) -> Result<(), SetError> {
        let (_, set) = field(key).ok_or_else(|| SetError::Unknown { key: key.into() })?;
        set(self, raw).map_err(|why| SetError::Value {
            key: key.into(),
            why,
        })
    }

    /// The setting `key`'s value, in the checked spelling ([`set`](Self::set)
    /// reads it back); None = unset (or no such setting). `env` gives its
    /// variables' names only: a value may be a key.
    pub fn get(&self, key: &str) -> Option<String> {
        field(key).and_then(|(get, _)| get(self))
    }
}

/// These settings, and the loader's own at their defaults (as [`Config::bare`]).
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
            engine => Some(WebSearchEngine::from_raw(engine)?),
        };
        Ok(())
    },
);

/// `env`: a `NAME=VALUE` adds that variable to the ones set before it. It
/// reads back as the names only: a value may be a key, and `haest settings`
/// shows what [`Config::get`] gives.
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
    "max_turn_usd" => settings.caps.max_turn_usd,
    "max_turn_tokens" => settings.caps.max_turn_tokens,
    "max_request_tokens" => settings.caps.max_request_tokens,
    "max_turn_steps" => settings.caps.max_turn_steps,
    "max_turn_minutes" => settings.caps.max_turn_minutes,
    "max_repeats" => settings.caps.max_repeats,
    "max_conversation_usd" => settings.caps.max_conversation_usd,
    "max_conversation_tokens" => settings.caps.max_conversation_tokens,
    "context_window" => settings.caps.context_window,
    "compact_at" => settings.caps.compact_at,
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
