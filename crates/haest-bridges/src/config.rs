//! [`HookSettings`], the `hooks` setting: hooks in another process bound
//! from a config file, one `[[hooks."<event>"]]` entry each, read and
//! checked here; bind.rs makes them the one `Hooks` a conversation adds.

use std::{collections::BTreeMap, fmt, time::Duration};

use haest_core::{
    __private::{expand_vars, unset_note},
    words::UnknownWord,
};
use haest_http::redacted_url;
use haest_mods::{ModEvent, OnError};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::SerializeMap};
use serde_json::{Map, Value};

#[cfg(any(feature = "shell", feature = "http"))]
mod bind;
#[cfg(test)]
mod tests;

#[cfg(any(feature = "shell", feature = "http"))]
pub use bind::HookHost;

haest_core::word_enum! {
    /// The JSON a configured hook speaks: an entry's `dialect`. haest may
    /// add one in a minor release.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum HookDialect ("hook dialect") {
        /// haest's own: the event's payload in, its decision out, every
        /// event ([`Haest`](crate::Haest)).
        #[default]
        Haest = "haest",
        /// Claude Code's hook JSON, so its hook scripts run as they are, on
        /// the events Claude Code has ([`Claude`](crate::Claude)).
        Claude = "claude",
    }
}

/// The events a `claude` entry binds: the ones Claude Code has (PreToolUse,
/// PostToolUse, UserPromptSubmit, SessionStart, Stop). `model.before` and
/// `model.after` have none (D42). The tests hold this to the events a
/// `Claude` hook implements.
const CLAUDE_EVENTS: &[ModEvent] = &[
    ModEvent::ToolBefore,
    ModEvent::ToolAfter,
    ModEvent::PromptSubmit,
    ModEvent::SystemPrompt,
    ModEvent::TurnEnd,
];

/// The events about one tool call, which a `matcher` narrows: haest-mods'
/// `ToolEvent`s. The tests hold this to the events bind.rs puts a matcher on.
const TOOL_EVENTS: &[ModEvent] = &[ModEvent::ToolBefore, ModEvent::ToolAfter];

/// The keys an entry takes.
const KEYS: &[&str] = &[
    "command", "url", "dialect", "matcher", "timeout", "on_error", "headers",
];

/// The `hooks` setting: commands and URLs bound to events from a config
/// file, so a CLI runs hook scripts and reaches hook services with no Rust.
/// Each entry is one [`HookEntry`]; together they make the same
/// [`Hooks`](haest_mods::Hooks) a Rust host binds by hand
/// ([`hooks`](Self::hooks)).
///
/// ```toml
/// # A Claude Code PreToolUse script, as it is, on Bash calls.
/// [[hooks."tool.before"]]
/// command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
/// dialect = "claude"
/// matcher = "Bash"
///
/// # A reviewer service that may keep a turn going, in haest's own JSON.
/// [[hooks."turn.end"]]
/// url = "https://review.internal/haest/turn-end"
/// headers = { authorization = "Bearer ${REVIEW_TOKEN}" }
/// timeout = 10
/// ```
///
/// An event is the protocol's own name, quoted where it has a dot
/// (`"tool.before"`): `[[hooks.tool.before]]` is a table `tool` in TOML, an
/// unknown event. Each event holds a list of entries, which bind in the
/// order written; a later config layer's entries come after an earlier
/// one's. The empty setting binds nothing.
///
/// Read from a config file, it checks each entry and refuses what it does
/// not take with a message naming the entry (`[[hooks."tool.before"]] #2`)
/// and never a header's value: an unknown event or key, both or neither of
/// `command` and `url`, `dialect = "claude"` on `model.before` or
/// `model.after` (Claude Code has no such event), a `matcher` on an event
/// that is not about a tool (or one that is a regular expression, which
/// Claude Code's matchers are and haest's are not), `headers` on a command.
/// Its Debug output shows each header by its name only and a URL without
/// its credentials.
#[derive(Clone, Default, PartialEq)]
pub struct HookSettings {
    /// Each event's entries, events in [`ModEvent::ALL`]'s order, none empty.
    events: Vec<(ModEvent, Vec<HookEntry>)>,
}

/// One `[[hooks."<event>"]]` entry: a command or a URL, and how it is bound.
///
/// | Key | Takes | Default |
/// |---|---|---|
/// | `command` | a command, run as `bash -c <command>` per event on the host, in the conversation's directory: the event's JSON on stdin, the decision on stdout ([`ShellHook`](crate::ShellHook)) | |
/// | `url` | an `http://` or `https://` URL, POSTed the event's JSON per event, the response's body the decision ([`HttpHook`](crate::HttpHook)) | |
/// | `dialect` | `haest` (the payload's own JSON, every event) or `claude` (Claude Code's hook JSON, not on `model.before` or `model.after`) | `haest` |
/// | `matcher` | the tools it is for, on `tool.before` and `tool.after` only: a name (`Bash`) or a prefix ending in `*` (`mcp__github__*`) | every tool |
/// | `timeout` | seconds it may take (`10`, `2.5`); past them it failed | 60 |
/// | `on_error` | what its failure does: `block` or `continue` | `block` on `tool.before`, else `continue` |
/// | `headers` | a `url` hook's headers, `{ name = "value" }`; `${NAME}` in a value is filled in from haest's environment when loaded ([`HookSettings::filled`]) | none |
///
/// Exactly one of `command` and `url`.
#[derive(Clone, PartialEq)]
pub struct HookEntry {
    target: Target,
    dialect: HookDialect,
    matcher: Option<String>,
    timeout: Option<Duration>,
    on_error: Option<OnError>,
    /// By name, in order; empty for a command.
    headers: Vec<(String, String)>,
}

/// What an entry runs.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Command(String),
    Url(String),
}

impl HookSettings {
    /// No entry: nothing binds.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Every entry with its event: the events in [`ModEvent::ALL`]'s order,
    /// each one's entries in the order they bind.
    pub fn entries(&self) -> impl Iterator<Item = (ModEvent, &HookEntry)> {
        self.events
            .iter()
            .flat_map(|(event, entries)| entries.iter().map(move |e| (*event, e)))
    }

    /// Each entry that POSTs, named as a refusal names it
    /// (`[[hooks."turn.end"]] #1`): what a host refuses from a config it
    /// does not trust, since a POST sends the conversation's events off the
    /// machine. haest-config refuses one in a project's file.
    pub fn url_entries(&self) -> Vec<String> {
        let mut named = Vec::new();
        for (event, entries) in &self.events {
            for (nth, entry) in (1..).zip(entries) {
                if entry.url().is_some() {
                    named.push(entry_name(*event, nth));
                }
            }
        }
        named
    }

    /// These settings with each `${NAME}` (or `${NAME:-default}`) in a
    /// header's value filled in from `lookup`, haest's environment: a token
    /// stays out of the config file. One with no value and no default stays
    /// as written, and a note says so: the server then refuses the request,
    /// which the entry's `on_error` handles (a `tool.before` hook refuses
    /// the call), rather than the hook going quietly missing.
    pub fn filled(&self, lookup: &dyn Fn(&str) -> Option<String>) -> (Self, Vec<String>) {
        let mut notes = Vec::new();
        let mut filled = self.clone();
        for (event, entries) in &mut filled.events {
            for (nth, entry) in (1..).zip(entries.iter_mut()) {
                for (name, value) in &mut entry.headers {
                    let (text, unset) = expand_vars(value, lookup);
                    if !unset.is_empty() {
                        let at = entry_name(*event, nth);
                        let unset = unset_note(&unset);
                        notes.push(format!("{at}: header {name} {unset}: sent as written"));
                    }
                    *value = text;
                }
            }
        }
        (filled, notes)
    }

    /// Each event's entries, checked: what [`Deserialize`] reads.
    fn read(table: BTreeMap<String, Value>) -> Result<Self, String> {
        let mut events = Vec::new();
        for (key, value) in table {
            // The word's own error lists every event; this one adds a hint.
            let Ok(event) = key.parse::<ModEvent>() else {
                return Err(unknown_event(&key));
            };
            let Value::Array(items) = value else {
                return Err(format!(
                    "\"{event}\" must be a list of tables: [[hooks.\"{event}\"]]"
                ));
            };
            let entries = (1..)
                .zip(items)
                .map(|(nth, item)| {
                    HookEntry::read(event, item)
                        .map_err(|why| format!("{}: {why}", entry_name(event, nth)))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !entries.is_empty() {
                events.push((event, entries));
            }
        }
        events.sort_by_key(|(event, _)| ModEvent::ALL.iter().position(|e| e == event));
        Ok(Self { events })
    }
}

/// How a message names the `nth` entry (from 1) of `event`: what the human
/// finds in the file.
fn entry_name(event: ModEvent, nth: usize) -> String {
    format!("[[hooks.\"{event}\"]] #{nth}")
}

/// Why `key` is no event, with the quoting a dotted name needs when `key` is
/// the start of one (`[[hooks.tool.before]]` reads as a table `tool`).
fn unknown_event(key: &str) -> String {
    let events = ModEvent::WORDS.join(", ");
    let dotted = ModEvent::WORDS.iter().find(|w| {
        w.strip_prefix(key)
            .is_some_and(|rest| rest.starts_with('.'))
    });
    match dotted {
        Some(word) => format!(
            "unknown event \"{key}\": quote an event's name, [[hooks.\"{word}\"]] (the events: {events})"
        ),
        None => format!("unknown event \"{key}\" (the events: {events})"),
    }
}

impl HookEntry {
    /// The command run per event, for a command hook.
    pub fn command(&self) -> Option<&str> {
        match &self.target {
            Target::Command(command) => Some(command),
            Target::Url(_) => None,
        }
    }

    /// The URL posted to per event, for an HTTP hook.
    pub fn url(&self) -> Option<&str> {
        match &self.target {
            Target::Url(url) => Some(url),
            Target::Command(_) => None,
        }
    }

    /// The JSON it speaks.
    pub fn dialect(&self) -> HookDialect {
        self.dialect
    }

    /// The tools it is for; None = every tool (and any event not about one).
    pub fn matcher(&self) -> Option<&str> {
        self.matcher.as_deref()
    }

    /// How long it may take; None = haest-mods' `DEFAULT_HOOK_TIMEOUT`.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// What its failure does; None = its event's default.
    pub fn on_error(&self) -> Option<OnError> {
        self.on_error
    }

    /// The names of the headers it sends.
    pub fn header_names(&self) -> impl Iterator<Item = &str> {
        self.headers.iter().map(|(name, _)| name.as_str())
    }

    /// One entry of `event`, checked; an error never holds a header's value.
    fn read(event: ModEvent, value: Value) -> Result<Self, String> {
        let Value::Object(fields) = value else {
            return Err(format!(
                "must be a table: [[hooks.\"{event}\"]] then command = \"...\" or url = \"...\""
            ));
        };
        if let Some(key) = fields.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!(
                "unknown key `{key}` (an entry takes {})",
                KEYS.join(", ")
            ));
        }
        let target = match (text(&fields, "command")?, text(&fields, "url")?) {
            (Some(command), None) if command.trim().is_empty() => {
                return Err("`command` is empty".into());
            }
            (Some(command), None) => Target::Command(command),
            (None, Some(url)) => Target::Url(checked_url(url)?),
            (Some(_), Some(_)) => {
                return Err(
                    "takes `command` or `url`, not both: a hook is a command or a URL".into(),
                );
            }
            (None, None) => {
                return Err(
                    "needs `command` (a command run per event) or `url` (a URL posted each event)"
                        .into(),
                );
            }
        };
        let dialect = match text(&fields, "dialect")? {
            None => HookDialect::Haest,
            Some(word) => word.parse().map_err(|e: UnknownWord| e.to_string())?,
        };
        if dialect == HookDialect::Claude && !CLAUDE_EVENTS.contains(&event) {
            let has: Vec<&str> = CLAUDE_EVENTS.iter().map(|e| e.as_str()).collect();
            return Err(format!(
                "{event} has no Claude Code event, so `dialect = \"claude\"` can't bind it (it binds {})",
                has.join(", ")
            ));
        }
        let matcher = text(&fields, "matcher")?;
        if let Some(pattern) = &matcher {
            if !TOOL_EVENTS.contains(&event) {
                return Err(format!(
                    "`matcher` picks tools, on tool.before and tool.after only: {event} is not about a tool call"
                ));
            }
            check_matcher(pattern)?;
        }
        let timeout = match fields.get("timeout") {
            None => None,
            Some(seconds) => Some(timeout(seconds)?),
        };
        let on_error = match text(&fields, "on_error")? {
            None => None,
            Some(word) => Some(word.parse().map_err(|e: UnknownWord| e.to_string())?),
        };
        let headers = match fields.get("headers") {
            None => Vec::new(),
            Some(_) if matches!(target, Target::Command(_)) => {
                return Err("`headers` are for a `url` hook only".into());
            }
            Some(headers) => read_headers(headers)?,
        };
        Ok(Self {
            target,
            dialect,
            matcher,
            timeout,
            on_error,
            headers,
        })
    }
}

/// `fields[key]` as text: absent is none, anything but a string is refused
/// (without echoing it).
fn text(fields: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match fields.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("`{key}` must be text")),
    }
}

/// A URL haest posts to: `http://` or `https://`. Not echoed: it may hold a
/// credential.
fn checked_url(url: String) -> Result<String, String> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Ok(url.trim().to_string())
    } else {
        Err("`url` must start with http:// or https://".into())
    }
}

/// haest's pattern: a tool's name, or a prefix ending in `*`. A regular
/// expression (Claude Code's matchers are) is refused: it would match no
/// tool, and a guard that never fires is worse than an error.
fn check_matcher(pattern: &str) -> Result<(), String> {
    const REGEX: &[char] = &['|', '(', ')', '[', ']', '{', '}', '^', '$', '+', '?', '\\'];
    let stars = pattern.matches('*').count();
    let regex = pattern.contains(REGEX)
        || pattern.ends_with(".*")
        || stars > 1
        || (stars == 1 && !pattern.ends_with('*'));
    if pattern.is_empty() {
        return Err("`matcher` is empty: leave it out for every tool".into());
    }
    if regex {
        return Err(format!(
            "`matcher = \"{pattern}\"` is not haest's pattern: a tool's name (`Bash`) or a prefix ending in `*` (`mcp__github__*`); Claude Code's regular expressions are not read (`Edit|Write` is two entries)"
        ));
    }
    Ok(())
}

/// `timeout`: seconds above 0, a whole number or not.
fn timeout(seconds: &Value) -> Result<Duration, String> {
    seconds
        .as_f64()
        .filter(|s| s.is_finite() && *s > 0.0)
        .and_then(|s| Duration::try_from_secs_f64(s).ok())
        .ok_or_else(|| "`timeout` must be a number of seconds above 0 (10, 2.5)".into())
}

/// `headers`: a table of names to text. An error names the header, never
/// its value.
fn read_headers(headers: &Value) -> Result<Vec<(String, String)>, String> {
    let Value::Object(headers) = headers else {
        return Err("`headers` must be a table: headers = { name = \"value\" }".into());
    };
    let mut read = Vec::new();
    for (name, value) in headers {
        let token = |c: char| c.is_ascii_graphic() && c != ':';
        if name.is_empty() || !name.chars().all(token) {
            return Err(format!("`{name}` is not a header name"));
        }
        let Value::String(value) = value else {
            return Err(format!("header `{name}` must be text"));
        };
        read.push((name.clone(), value.clone()));
    }
    Ok(read)
}

impl<'de> Deserialize<'de> for HookSettings {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let table = BTreeMap::<String, Value>::deserialize(d)?;
        Self::read(table).map_err(D::Error::custom)
    }
}

/// Each event, a list of its entries, as a config file writes them.
impl Serialize for HookSettings {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.events.len()))?;
        for (event, entries) in &self.events {
            map.serialize_entry(event.as_str(), entries)?;
        }
        map.end()
    }
}

/// The keys a config file writes: a default left out.
impl Serialize for HookEntry {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        match &self.target {
            Target::Command(command) => map.serialize_entry("command", command)?,
            Target::Url(url) => map.serialize_entry("url", url)?,
        }
        if self.dialect != HookDialect::default() {
            map.serialize_entry("dialect", &self.dialect)?;
        }
        if let Some(matcher) = &self.matcher {
            map.serialize_entry("matcher", matcher)?;
        }
        if let Some(timeout) = self.timeout {
            map.serialize_entry("timeout", &Seconds(timeout))?;
        }
        if let Some(on_error) = self.on_error {
            map.serialize_entry("on_error", &on_error)?;
        }
        if !self.headers.is_empty() {
            map.serialize_entry("headers", &Headers(&self.headers))?;
        }
        map.end()
    }
}

/// A duration in seconds: a whole number when it is one.
struct Seconds(Duration);

impl Serialize for Seconds {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Seconds(duration) = self;
        if duration.subsec_nanos() == 0 {
            s.serialize_u64(duration.as_secs())
        } else {
            s.serialize_f64(duration.as_secs_f64())
        }
    }
}

/// Headers as the table a file writes.
struct Headers<'a>(&'a [(String, String)]);

impl Serialize for Headers<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

/// Each event and its entries; see [`HookEntry`]'s Debug.
impl fmt::Debug for HookSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.events.iter().map(|(e, entries)| (e.as_str(), entries)))
            .finish()
    }
}

/// The URL without its credentials, each header by its name only: a value
/// may be a token.
impl fmt::Debug for HookEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("HookEntry");
        match &self.target {
            Target::Command(command) => s.field("command", command),
            Target::Url(url) => s.field("url", &redacted_url(url)),
        };
        let headers: Vec<&str> = self.header_names().collect();
        s.field("dialect", &self.dialect)
            .field("matcher", &self.matcher)
            .field("timeout", &self.timeout)
            .field("on_error", &self.on_error)
            .field("headers", &headers)
            .finish()
    }
}
