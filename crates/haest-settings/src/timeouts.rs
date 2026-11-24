//! [`TimeoutSettings`], the `[timeouts]` setting, and [`LimitSettings`],
//! `[limits]`: how long haest waits on a hook, on the human, on a shell
//! command, on a model and on its JavaScript, and how much memory that
//! JavaScript gets.

use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};

use crate::units::{de_duration, de_memory, duration_text, memory_text};

/// The `[timeouts]` setting: how long haest waits before it gives up. Every
/// value is text with its unit (`"500ms"`, `"30s"`, `"10m"`, `"1.5h"`); a
/// number alone is refused. Each default is generous, and each is the
/// one haest used before the setting existed but `javascript.code`, which
/// was a minute.
///
/// ```toml
/// [timeouts]
/// hook          = "60s"   # every [[hooks.<event>]] entry; an entry's own `timeout` wins
/// ask           = "10m"   # a human's Allow/Deny prompt; unanswered, it is a Deny
/// bash          = "2m"    # a Bash call that names no timeout
/// bash_max      = "10m"   # the most a Bash call may ask for; the sandbox shell's ceiling too
/// model_headers = "10m"   # one try of a model call, until the provider starts answering
/// mcp_connect   = "15s"   # an MCP server's start, and again its tool list
/// mcp_call      = "10m"   # one call to an MCP server's tool
/// web_fetch     = "30s"   # one WebFetch call, redirects and body included
///
/// [timeouts.javascript]
/// code      = "10m"    # a Code tool run, the tools it calls included
/// sandbox   = "5m"     # the sandbox `js` command
/// mod_load  = "30s"    # a mod file's default export, as the mod loads
/// mod_call  = "60s"    # one call into a mod file; the human's time on haest.ask excluded
/// mod_slice = "10s"    # a mod file's code without a pause: stops a CPU loop
/// ```
///
/// ```
/// use std::time::Duration;
///
/// use haest_settings::{JsTimeouts, TimeoutSettings};
///
/// let patient = TimeoutSettings {
///     javascript: JsTimeouts { code: Duration::from_secs(30 * 60), ..JsTimeouts::default() },
///     ..TimeoutSettings::default()
/// };
/// assert_eq!(patient.hook, Duration::from_secs(60));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeoutSettings {
    /// How long a `[[hooks.<event>]]` entry (a command or a URL) has to
    /// answer when it sets no `timeout` of its own; past it, the hook
    /// failed and its `on_error` decides. Default a minute.
    #[serde(deserialize_with = "de_duration")]
    pub hook: Duration,
    /// How long an Allow/Deny prompt waits on the human (the approval mod's,
    /// a mod's `ask`); unanswered, it is a Deny. Default ten minutes.
    #[serde(deserialize_with = "de_duration")]
    pub ask: Duration,
    /// A Bash call's timeout when it names none; past it the command is
    /// killed. Default two minutes.
    #[serde(deserialize_with = "de_duration")]
    pub bash: Duration,
    /// The most a Bash call may ask for, and the longest the sandbox shell
    /// runs one call; never below `bash` ([`bash_ceiling`](Self::bash_ceiling)).
    /// Default ten minutes.
    #[serde(deserialize_with = "de_duration")]
    pub bash_max: Duration,
    /// How long one try of a model call waits for the provider to start
    /// answering (its status and headers); past it the try fails and is
    /// retried as a dropped connection is. A stream that has started is
    /// not cut. Default ten minutes.
    #[serde(deserialize_with = "de_duration")]
    pub model_headers: Duration,
    /// How long an MCP server has to start and answer `initialize`, and
    /// again to list its tools; past it the server did not start. A server
    /// run with `npx` or `uvx` may need longer the first time. Default 15
    /// seconds.
    #[serde(deserialize_with = "de_duration")]
    pub mcp_connect: Duration,
    /// How long one call to an MCP server's tool may run; past it the call
    /// fails and is cancelled on the server. Default ten minutes.
    #[serde(deserialize_with = "de_duration")]
    pub mcp_call: Duration,
    /// How long one WebFetch call may take, every redirect and the body
    /// included. Default 30 seconds.
    #[serde(deserialize_with = "de_duration")]
    pub web_fetch: Duration,
    /// haest's JavaScript: the Code tool, the sandbox `js` command, mod
    /// files.
    pub javascript: JsTimeouts,
}

impl Default for TimeoutSettings {
    fn default() -> Self {
        Self {
            hook: Duration::from_secs(60),
            ask: Duration::from_secs(10 * 60),
            bash: Duration::from_secs(2 * 60),
            bash_max: Duration::from_secs(10 * 60),
            model_headers: Duration::from_secs(10 * 60),
            mcp_connect: Duration::from_secs(15),
            mcp_call: Duration::from_secs(10 * 60),
            web_fetch: Duration::from_secs(30),
            javascript: JsTimeouts::default(),
        }
    }
}

impl TimeoutSettings {
    /// The most a Bash call may take: `bash_max`, or `bash` when that is
    /// longer, so `bash = "20m"` alone is never cut to ten minutes.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use haest_settings::TimeoutSettings;
    ///
    /// let long = TimeoutSettings { bash: Duration::from_secs(1_200), ..TimeoutSettings::default() };
    /// assert_eq!(long.bash_ceiling(), Duration::from_secs(1_200));
    /// assert_eq!(TimeoutSettings::default().bash_ceiling(), Duration::from_secs(600));
    /// ```
    pub fn bash_ceiling(&self) -> Duration {
        self.bash_max.max(self.bash)
    }
}

/// `[timeouts.javascript]`: how long each kind of JavaScript run may take.
/// A run past its time is stopped, mid-loop too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JsTimeouts {
    /// A Code tool run, wall clock, the tools it calls included. The model
    /// may ask for less on a call, never for more. Default ten minutes.
    #[serde(deserialize_with = "de_duration")]
    pub code: Duration,
    /// The sandbox `js` command (`js script.js`, `node -e`), and never
    /// past the shell call it runs in. Default five minutes.
    #[serde(deserialize_with = "de_duration")]
    pub sandbox: Duration,
    /// How long a mod file's default export has to return as the mod
    /// loads. Default 30 seconds.
    #[serde(deserialize_with = "de_duration")]
    pub mod_load: Duration,
    /// How long one call into a loaded mod file (a handler, a tool, a
    /// command) has to settle, awaiting included; the time a `haest.ask`
    /// waits on the human does not count. Past it that call fails and the
    /// mod runs on. Default a minute.
    #[serde(deserialize_with = "de_duration")]
    pub mod_call: Duration,
    /// The longest a mod file's code may run without a pause (awaiting
    /// does not count): what stops a CPU loop in a mod. Past it every call
    /// into the mod in flight fails. Default ten seconds.
    #[serde(deserialize_with = "de_duration")]
    pub mod_slice: Duration,
}

impl Default for JsTimeouts {
    fn default() -> Self {
        Self {
            code: Duration::from_secs(10 * 60),
            sandbox: Duration::from_secs(5 * 60),
            mod_load: Duration::from_secs(30),
            mod_call: Duration::from_secs(60),
            mod_slice: Duration::from_secs(10),
        }
    }
}

/// The `[limits]` setting: how much haest's JavaScript may hold. Every value
/// is text with its unit (`"64MiB"`, `"1GiB"`, `"500MB"`), 1MiB at least; a
/// number alone is refused.
///
/// ```toml
/// [limits.javascript]
/// code_memory    = "64MiB"    # a Code tool run's heap
/// sandbox_memory = "64MiB"    # the sandbox `js` command's heap
/// mod_memory     = "128MiB"   # each mod file's heap, for as long as it is loaded
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitSettings {
    /// haest's JavaScript: the Code tool, the sandbox `js` command, mod
    /// files.
    pub javascript: JsLimits,
}

/// `[limits.javascript]`: each JavaScript engine's heap, in bytes. An
/// allocation past it is an out-of-memory error inside the script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JsLimits {
    /// A Code tool run's heap. Default 64 MiB.
    #[serde(deserialize_with = "de_memory")]
    pub code_memory: usize,
    /// The sandbox `js` command's heap. Default 64 MiB.
    #[serde(deserialize_with = "de_memory")]
    pub sandbox_memory: usize,
    /// Each mod file's heap, for as long as it is loaded. Default 128 MiB.
    #[serde(deserialize_with = "de_memory")]
    pub mod_memory: usize,
}

impl Default for JsLimits {
    fn default() -> Self {
        Self {
            code_memory: 64 << 20,
            sandbox_memory: 64 << 20,
            mod_memory: 128 << 20,
        }
    }
}

/// Writes `entries` that differ from their default, each as its unit
/// string, then `nested` when it differs from its own: a table at its
/// defaults writes nothing, so it reads as unset.
fn write_changed<S: Serializer, N: Serialize + PartialEq>(
    s: S,
    entries: &[(&str, String, String)],
    nested: Option<(&str, &N, &N)>,
) -> Result<S::Ok, S::Error> {
    let mut map = s.serialize_map(None)?;
    for (key, value, default) in entries {
        if value != default {
            map.serialize_entry(key, value)?;
        }
    }
    if let Some((key, value, default)) = nested
        && value != default
    {
        map.serialize_entry(key, value)?;
    }
    map.end()
}

/// Every key off its default, as its unit string.
impl Serialize for TimeoutSettings {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let d = Self::default();
        let t = duration_text;
        write_changed(
            s,
            &[
                ("hook", t(self.hook), t(d.hook)),
                ("ask", t(self.ask), t(d.ask)),
                ("bash", t(self.bash), t(d.bash)),
                ("bash_max", t(self.bash_max), t(d.bash_max)),
                ("model_headers", t(self.model_headers), t(d.model_headers)),
                ("mcp_connect", t(self.mcp_connect), t(d.mcp_connect)),
                ("mcp_call", t(self.mcp_call), t(d.mcp_call)),
                ("web_fetch", t(self.web_fetch), t(d.web_fetch)),
            ],
            Some(("javascript", &self.javascript, &d.javascript)),
        )
    }
}

/// Every key off its default, as its unit string.
impl Serialize for JsTimeouts {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let d = Self::default();
        let t = duration_text;
        write_changed::<S, ()>(
            s,
            &[
                ("code", t(self.code), t(d.code)),
                ("sandbox", t(self.sandbox), t(d.sandbox)),
                ("mod_load", t(self.mod_load), t(d.mod_load)),
                ("mod_call", t(self.mod_call), t(d.mod_call)),
                ("mod_slice", t(self.mod_slice), t(d.mod_slice)),
            ],
            None,
        )
    }
}

/// `javascript` when it is off its default.
impl Serialize for LimitSettings {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let d = Self::default();
        write_changed(
            s,
            &[],
            Some(("javascript", &self.javascript, &d.javascript)),
        )
    }
}

/// Every key off its default, as its unit string.
impl Serialize for JsLimits {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let d = Self::default();
        let m = memory_text;
        write_changed::<S, ()>(
            s,
            &[
                ("code_memory", m(self.code_memory), m(d.code_memory)),
                (
                    "sandbox_memory",
                    m(self.sandbox_memory),
                    m(d.sandbox_memory),
                ),
                ("mod_memory", m(self.mod_memory), m(d.mod_memory)),
            ],
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeouts(text: &str) -> Result<TimeoutSettings, String> {
        toml::from_str(text).map_err(|e| e.message().to_string())
    }

    fn limits(text: &str) -> Result<LimitSettings, String> {
        toml::from_str(text).map_err(|e| e.message().to_string())
    }

    #[test]
    fn an_empty_table_is_every_default_and_writes_nothing() {
        assert_eq!(timeouts("").unwrap(), TimeoutSettings::default());
        assert_eq!(limits("").unwrap(), LimitSettings::default());
        let written = toml::to_string(&TimeoutSettings::default()).unwrap();
        assert_eq!(written.trim(), "");
        let written = toml::to_string(&LimitSettings::default()).unwrap();
        assert_eq!(written.trim(), "");
    }

    #[test]
    fn a_table_sets_only_what_it_names_and_writes_back_only_that() {
        let t = timeouts("hook = \"30s\"\n[javascript]\ncode = \"1.5h\"").unwrap();
        assert_eq!(t.hook, Duration::from_secs(30));
        assert_eq!(t.javascript.code, Duration::from_secs(5_400));
        assert_eq!(t.ask, TimeoutSettings::default().ask);
        assert_eq!(t.javascript.mod_slice, JsTimeouts::default().mod_slice);
        let written = toml::to_string(&t).unwrap();
        assert_eq!(timeouts(&written).unwrap(), t, "{written}");
        assert!(
            !written.contains("ask") && written.contains("\"90m\""),
            "{written}"
        );
        let t = timeouts(
            "bash = \"5m\"\nbash_max = \"1h\"\nmodel_headers = \"90s\"\nmcp_call = \"30m\"\nmcp_connect = \"2m\"\nweb_fetch = \"45s\"",
        )
        .unwrap();
        let mins = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(
            (t.bash, t.bash_max, t.model_headers, t.mcp_call),
            (mins(5), mins(60), Duration::from_secs(90), mins(30))
        );
        assert_eq!(
            (t.mcp_connect, t.web_fetch),
            (mins(2), Duration::from_secs(45))
        );
        assert_eq!(timeouts(&toml::to_string(&t).unwrap()).unwrap(), t);

        let l = limits("[javascript]\nmod_memory = \"1GiB\"").unwrap();
        assert_eq!(l.javascript.mod_memory, 1 << 30);
        assert_eq!(l.javascript.code_memory, 64 << 20);
        let written = toml::to_string(&l).unwrap();
        assert_eq!(limits(&written).unwrap(), l, "{written}");
        assert!(!written.contains("code_memory"), "{written}");
    }

    #[test]
    fn a_typo_or_a_bare_number_is_refused() {
        let why = timeouts("hok = \"30s\"").unwrap_err();
        assert!(why.contains("hok"), "{why}");
        let why = timeouts("[javascript]\ncode = 600").unwrap_err();
        assert!(why.contains("600 has no unit"), "{why}");
        let why = limits("[javascript]\ncode_memory = \"512KiB\"").unwrap_err();
        assert!(why.contains("1MiB at least"), "{why}");
        let why = limits("[javascript]\nmemory = \"64MiB\"").unwrap_err();
        assert!(why.contains("memory"), "{why}");
    }
}
