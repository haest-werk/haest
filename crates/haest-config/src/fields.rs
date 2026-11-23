//! Every setting as a field. [`Config`] holds the runtime's [`Settings`] and
//! the ones only a loader reads; [`Config::set`] and [`Config::get`] go by the
//! setting's name, in the one spelling [`crate::value::check`] gives a value,
//! so what a file says and what `haest settings` shows are the same text.

mod raw;

use std::{collections::BTreeMap, fmt, path::PathBuf};

use haest_core::provider::WebSearchEngine;
use haest_mods::HookSettings;
use haest_providers::http::__private::UrlDebug;
use haest_settings::Settings;

pub(crate) use self::raw::Raw;
use crate::{
    SetError,
    registry::FIELDS,
    value::{var, var_parts},
};

haest_core::word_enum! {
    /// What a process haest starts (bash, a background job, an MCP server) gets of
    /// haest's own environment: the `env_passthrough` setting. Codex's
    /// `shell_environment_policy.inherit`. What the `env` setting passes on
    /// purpose comes on top, whichever it is. haest may add a policy in a
    /// minor release.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    #[non_exhaustive]
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

haest_core::word_enum! {
    /// What a control socket lets a program on this machine do: the
    /// `control_socket` setting. Off, it is not opened at all. haest may add
    /// a level in a minor release, as the transport's grants grow.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum ControlSocket ("control_socket") {
        /// No socket.
        #[default]
        Off = "off",
        /// Read the conversation: its stream, a snapshot, the history.
        Watch = "watch",
        /// Read it and drive it: turns, interrupts, changes, approval answers.
        Command = "command",
    }
}

haest_core::word_enum! {
    /// What a broker may do over the link: the `broker_access` setting.
    /// haest may add a level in a minor release, as the transport's grants
    /// grow.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum BrokerAccess ("broker_access") {
        /// Read the conversation: its stream, a snapshot, the history.
        #[default]
        Watch = "watch",
        /// Read it and drive it: turns, interrupts, changes, approval answers.
        Command = "command",
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
/// setting's name. Its Debug output shows `env` by name only, as `get` does.
#[derive(Clone, PartialEq)]
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
    /// Frames a host's session keeps for a peer that catches up, besides the
    /// running turn (haest-session's `HubOptions::ring`). Default 1000.
    pub event_ring: usize,
    /// Be driven by a parent process over stdin and stdout, with no
    /// terminal (haest-transport's `Stdio`). The environment and flags only.
    pub stdio: bool,
    /// The control socket another program on this machine watches (or
    /// drives) the conversation through. Default off.
    pub control_socket: ControlSocket,
    /// Where control sockets and their discovery files go; None =
    /// `<data_dir>/control` ([`Config::control_dir`]).
    pub control_dir: Option<PathBuf>,
    /// The broker the conversation links to, `ws://` or `wss://`; None = no
    /// link. Its token is a key (`HAEST_BROKER_TOKEN`), never a setting.
    ///
    /// The loader refuses one with an `@` past its host ([`Kind::Url`]):
    /// `wss://agent:12/34@broker.example/agents` reads to a URL parser as
    /// the host `agent` on port 12, which would be sent the rest of the
    /// password in the path. A password goes in percent-encoded (`/` as
    /// `%2F`); the link never sends it, nor the user. It refuses one with
    /// no host too (`wss://:8443/agents`, what an empty `${BROKER_HOST}`
    /// makes). [`Config::set`] writes it unchecked; the broker link checks
    /// it again before it dials.
    ///
    /// [`Kind::Url`]: crate::Kind::Url
    pub broker_url: Option<String>,
    /// What the broker may do over the link. Default watch.
    pub broker_access: BrokerAccess,
    /// `hooks`: commands and URLs bound to events, every layer's entries
    /// after the ones before, as written. [`crate::Loaded::hooks`] has
    /// their headers' `${NAME}`s filled in. Default none.
    pub hooks: HookSettings,
}

impl Config {
    /// Bare haest working in `cwd` with `home` as the home dir
    /// ([`Settings::bare`]), and the loader's own settings at their defaults:
    /// no profile, no trusted dirs, `env_passthrough = core`, no `env`, `mcp`
    /// off, no routing, a ring of 1000 frames, not driven over stdio, no
    /// control socket, no broker link, no hooks.
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
    /// # fn main() -> Result<(), SetError> {
    /// let mut c = Config::bare("/work", "/home/me");
    /// c.set("tools", "grep,read")?;
    /// c.set("max_turn_usd", "5")?;
    /// assert_eq!(c.get("tools").as_deref(), Some("read,grep"));
    /// assert_eq!(c.settings.caps.max_turn_usd, 5.0);
    ///
    /// assert!(matches!(c.set("tool", "read"), Err(SetError::Unknown { .. })));
    /// assert!(matches!(c.set("max_turn_steps", "2.5"), Err(SetError::Value { .. })));
    /// # Ok(())
    /// # }
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

    /// Where control sockets go: `control_dir`, else `<data_dir>/control`.
    ///
    /// ```
    /// use haest_config::Config;
    ///
    /// let c = Config::bare("/w", "/home/me");
    /// assert_eq!(c.control_dir(), std::path::Path::new("/home/me/.local/share/haest/control"));
    /// ```
    pub fn control_dir(&self) -> PathBuf {
        self.control_dir
            .clone()
            .unwrap_or_else(|| self.settings.data_dir.join("control"))
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
            event_ring: 1000,
            stdio: false,
            control_socket: ControlSocket::Off,
            control_dir: None,
            broker_url: None,
            broker_access: BrokerAccess::Watch,
            hooks: HookSettings::default(),
        }
    }
}

/// Every field, but `env` by its variables' names (a value may be a key;
/// [`Config::get`] gives the names too), `broker_url` without its
/// userinfo, query values or fragment ([`Config::get`] gives it as set),
/// and `hooks` with each header by its name ([`Config::get`] too) and each
/// URL without its credentials.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Config {
            settings,
            profile,
            trusted_dirs,
            env_passthrough,
            env,
            mcp,
            provider_routing,
            event_ring,
            stdio,
            control_socket,
            control_dir,
            broker_url,
            broker_access,
            hooks,
        } = self;
        f.debug_struct("Config")
            .field("settings", settings)
            .field("profile", profile)
            .field("trusted_dirs", trusted_dirs)
            .field("env_passthrough", env_passthrough)
            .field("env", &env.keys().collect::<Vec<_>>())
            .field("mcp", mcp)
            .field("provider_routing", provider_routing)
            .field("event_ring", event_ring)
            .field("stdio", stdio)
            .field("control_socket", control_socket)
            .field("control_dir", control_dir)
            .field("broker_url", &broker_url.as_deref().map(UrlDebug))
            .field("broker_access", broker_access)
            .field("hooks", hooks)
            .finish()
    }
}

/// How [`Config::get`] reads one setting.
pub(crate) type Get = fn(&Config) -> Option<String>;
/// How [`Config::set`] writes one setting.
pub(crate) type Set = fn(&mut Config, &str) -> Result<(), String>;

/// `web_search`: `off` is a value (no search), not unset, so it does not go
/// by the `Option` rule every other optional setting follows.
pub(crate) const WEB_SEARCH: (Get, Set) = (
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
pub(crate) const ENV: (Get, Set) = (
    |c| (!c.env.is_empty()).then(|| c.env.keys().cloned().collect::<Vec<_>>().join(",")),
    |c, raw| {
        let entry = var(raw)?;
        let (name, value) = var_parts(&entry);
        c.env.insert(name.into(), value.into());
        Ok(())
    },
);

/// The setting `key`'s get and set, from the registry's one row for it.
fn field(key: &str) -> Option<(Get, Set)> {
    FIELDS.iter().find(|(k, _)| *k == key).map(|(_, f)| *f)
}
