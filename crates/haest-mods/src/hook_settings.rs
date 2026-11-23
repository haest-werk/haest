//! [`HookSettings`], the `hooks` setting: hooks in another process bound
//! from a config file (or built in Rust), one `[[hooks."<event>"]]` entry
//! each, read and checked here. haest-bridges' `HookHost::bind` makes them
//! the one [`Hooks`](crate::Hooks) a conversation adds; this crate runs no
//! command and posts nothing.

use std::{collections::BTreeMap, fmt, time::Duration};

use haest_core::{
    __private::{
        BadDuration, BadPattern, DurationIn, PatternOf, TimeUnit, check_pattern, duration_in,
        expand_vars, unset_note,
    },
    words::UnknownWord,
};
use haest_http::redacted_url;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::SerializeMap};
use serde_json::{Map, Value};

use crate::{events::ModEvent, hook::OnError};

#[cfg(test)]
mod tests;

haest_core::word_enum! {
    /// The JSON a configured hook speaks: an entry's `dialect`. haest may
    /// add one in a minor release.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum HookDialect ("hook dialect") {
        /// haest's own: the event's payload in, its decision out, every
        /// event (haest-bridges' `Haest`).
        #[default]
        Haest = "haest",
        /// Claude Code's hook JSON, so its hook scripts run as they are, on
        /// the events Claude Code has (haest-bridges' `Claude`).
        Claude = "claude",
    }
}

/// The events a `claude` entry binds: the ones Claude Code has (PreToolUse,
/// PostToolUse, UserPromptSubmit, SessionStart, Stop). `model.before` and
/// `model.after` have none (D42). haest-bridges' tests hold this to the
/// events its `Claude` hook implements.
const CLAUDE_EVENTS: &[ModEvent] = &[
    ModEvent::ToolBefore,
    ModEvent::ToolAfter,
    ModEvent::PromptSubmit,
    ModEvent::SystemPrompt,
    ModEvent::TurnEnd,
];

/// The events about one tool call, which a `matcher` narrows: the
/// [`ToolEvent`](crate::ToolEvent)s. haest-bridges' tests hold this to the
/// events it puts a matcher on.
const TOOL_EVENTS: &[ModEvent] = &[ModEvent::ToolBefore, ModEvent::ToolAfter];

/// The keys an entry takes.
const KEYS: &[&str] = &[
    "command", "url", "dialect", "matcher", "timeout", "on_error", "headers",
];

/// The `hooks` setting: commands and URLs bound to events from a config
/// file, so a CLI runs hook scripts and reaches hook services with no Rust.
/// Each entry is one [`HookEntry`]; haest-bridges' `HookHost::bind` makes
/// them the same [`Hooks`](crate::Hooks) a Rust host binds by hand.
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
///
/// A Rust host builds the same without a file, each entry checked as a
/// file's is ([`with_entry`](Self::with_entry)):
///
/// ```
/// use std::time::Duration;
///
/// use haest_mods::{HookDialect, HookEntry, HookSettings, ModEvent};
///
/// # fn main() -> Result<(), haest_mods::HookEntryError> {
/// let hooks = HookSettings::default()
///     .with_entry(
///         ModEvent::ToolBefore,
///         HookEntry::shell("sh ~/.config/haest/hooks/no-rm-rf.sh")
///             .with_dialect(HookDialect::Claude)
///             .with_matcher("Bash"),
///     )?
///     .with_entry(
///         ModEvent::TurnEnd,
///         HookEntry::http("https://review.internal/haest/turn-end")
///             .with_header("authorization", "Bearer ${REVIEW_TOKEN}")
///             .with_timeout(Duration::from_secs(10)),
///     )?;
/// assert_eq!(hooks.entries().count(), 2);
///
/// // A matcher on an event that is not about a tool is refused, as in a file.
/// let wrong = HookSettings::default()
///     .with_entry(ModEvent::TurnEnd, HookEntry::shell("sh check.sh").with_matcher("Bash"));
/// assert!(wrong.unwrap_err().to_string().contains("tool.before and tool.after only"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default, PartialEq)]
pub struct HookSettings {
    /// Each event's entries, events in [`ModEvent::ALL`]'s order, none empty.
    events: Vec<(ModEvent, Vec<HookEntry>)>,
}

/// One `[[hooks."<event>"]]` entry: a command or a URL, and how it is bound.
///
/// | Key | Takes | Default |
/// |---|---|---|
/// | `command` | a command, run as `bash -c <command>` per event on the host, in the conversation's directory: the event's JSON on stdin, the decision on stdout (haest-bridges' `ShellHook`) | |
/// | `url` | an `http://` or `https://` URL, POSTed the event's JSON per event, the response's body the decision (haest-bridges' `HttpHook`) | |
/// | `dialect` | `haest` (the payload's own JSON, every event) or `claude` (Claude Code's hook JSON, not on `model.before` or `model.after`) | `haest` |
/// | `matcher` | the tools it is for, on `tool.before` and `tool.after` only: a name (`Bash`) or a prefix ending in `*` (`mcp__github__*`) | every tool |
/// | `timeout` | seconds it may take (`10`, `2.5`); past them it failed. Claude Code's key, as it is: every other time setting that takes a number takes minutes, and the cache's TTLs are the words `5m` and `1h` | 60 |
/// | `on_error` | what its failure does: `block` or `continue` | `block` on `tool.before`, else `continue` |
/// | `headers` | a `url` hook's headers, `{ name = "value" }`; `${NAME}` in a value is filled in from haest's environment when loaded ([`HookSettings::filled`]) | none |
///
/// Exactly one of `command` and `url`: [`shell`](Self::shell) or
/// [`http`](Self::http) in Rust, then the `with_*` setters for the rest.
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

/// Why [`HookSettings::with_entry`] refused an entry: what a config file's
/// entry is refused for, said the same way. It never holds a header's
/// value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a {event} hook: {why}")]
pub struct HookEntryError {
    event: ModEvent,
    why: String,
}

impl HookEntryError {
    /// The event the entry was for.
    pub fn event(&self) -> ModEvent {
        self.event
    }
}

impl HookSettings {
    /// No entry: nothing binds.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// These settings with `entry` bound to `event`, after the event's
    /// entries so far: what a config file's `[[hooks."<event>"]]` adds.
    ///
    /// # Errors
    ///
    /// [`HookEntryError`] when a config file would refuse the entry: an
    /// empty command, a URL that is not `http://` or `https://`, the
    /// `claude` dialect on an event Claude Code lacks, a matcher on an
    /// event that is not about a tool or one that is not haest's pattern, a
    /// zero timeout, headers on a command, a header name that is not one.
    pub fn with_entry(mut self, event: ModEvent, entry: HookEntry) -> Result<Self, HookEntryError> {
        entry
            .check(event)
            .map_err(|why| HookEntryError { event, why })?;
        match self.events.iter_mut().find(|(e, _)| *e == event) {
            Some((_, entries)) => entries.push(entry),
            None => {
                self.events.push((event, vec![entry]));
                self.events
                    .sort_by_key(|(event, _)| ModEvent::ALL.iter().position(|e| e == event));
            }
        }
        Ok(self)
    }

    /// Every entry with its event: the events in [`ModEvent::ALL`]'s order,
    /// each one's entries in the order they bind.
    pub fn entries(&self) -> impl Iterator<Item = (ModEvent, &HookEntry)> {
        self.events
            .iter()
            .flat_map(|(event, entries)| entries.iter().map(move |e| (*event, e)))
    }

    /// [`entries`](Self::entries), each with the name a message gives it:
    /// `[[hooks."turn.end"]] #2` is the second entry of `turn.end`, as a
    /// config file numbers them.
    pub fn named_entries(&self) -> impl Iterator<Item = (ModEvent, String, &HookEntry)> {
        self.events.iter().flat_map(|(event, entries)| {
            (1..)
                .zip(entries)
                .map(move |(nth, e)| (*event, entry_name(*event, nth), e))
        })
    }

    /// Each entry that POSTs, named as a refusal names it
    /// (`[[hooks."turn.end"]] #1`): what a host refuses from a config it
    /// does not trust, since a POST sends the conversation's events off the
    /// machine. haest-config refuses one in a project's file.
    pub fn url_entries(&self) -> Vec<String> {
        (self.named_entries())
            .filter(|(_, _, entry)| entry.url().is_some())
            .map(|(_, name, _)| name)
            .collect()
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
    /// A hook that runs `command` per event (a config file's `command`),
    /// with the defaults: haest's dialect, every tool, the event's timeout
    /// and `on_error`.
    pub fn shell(command: impl Into<String>) -> Self {
        Self::of(Target::Command(command.into()))
    }

    /// A hook that POSTs each event to `url` (a config file's `url`), with
    /// the defaults and no headers.
    pub fn http(url: impl Into<String>) -> Self {
        Self::of(Target::Url(url.into().trim().to_string()))
    }

    fn of(target: Target) -> Self {
        Self {
            target,
            dialect: HookDialect::Haest,
            matcher: None,
            timeout: None,
            on_error: None,
            headers: Vec::new(),
        }
    }

    /// It speaks `dialect`.
    #[must_use]
    pub fn with_dialect(mut self, dialect: HookDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// On `tool.before` or `tool.after`, it is asked only about the tools
    /// `pattern` matches: a name (`Bash`) or a prefix ending in `*`.
    #[must_use]
    pub fn with_matcher(mut self, pattern: impl Into<String>) -> Self {
        self.matcher = Some(pattern.into());
        self
    }

    /// It may take up to `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// What its failure does.
    #[must_use]
    pub fn with_on_error(mut self, on_error: OnError) -> Self {
        self.on_error = Some(on_error);
        self
    }

    /// A URL hook sends the header `name: value` (after the ones set
    /// before it).
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

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

    /// How long it may take; None = [`DEFAULT_HOOK_TIMEOUT`](crate::DEFAULT_HOOK_TIMEOUT).
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

    /// The headers it sends, name and value, in order: what a bridge puts
    /// on the request. A value may be a key: never show it.
    pub fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        (self.headers.iter()).map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// One entry of `event` as a file writes it, checked; an error never
    /// holds a header's value.
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
        let mut entry = match (text(&fields, "command")?, text(&fields, "url")?) {
            (Some(command), None) => Self::shell(command),
            (None, Some(url)) => Self::http(url),
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
        if let Some(word) = text(&fields, "dialect")? {
            entry.dialect = word.parse().map_err(|e: UnknownWord| e.to_string())?;
        }
        entry.matcher = text(&fields, "matcher")?;
        if let Some(seconds) = fields.get("timeout") {
            entry.timeout = Some(timeout(seconds)?);
        }
        if let Some(word) = text(&fields, "on_error")? {
            entry.on_error = Some(word.parse().map_err(|e: UnknownWord| e.to_string())?);
        }
        if let Some(headers) = fields.get("headers") {
            entry.headers = read_headers(headers)?;
        }
        entry.check(event)?;
        Ok(entry)
    }

    /// Whether `event` may bind this entry: the rules a file's entry and a
    /// Rust host's are held to alike. An error never holds a header's value
    /// or the URL.
    fn check(&self, event: ModEvent) -> Result<(), String> {
        match &self.target {
            Target::Command(command) if command.trim().is_empty() => {
                return Err("`command` is empty".into());
            }
            Target::Url(url) => check_url(url)?,
            Target::Command(_) => {}
        }
        if self.dialect == HookDialect::Claude && !CLAUDE_EVENTS.contains(&event) {
            let has: Vec<&str> = CLAUDE_EVENTS.iter().map(|e| e.as_str()).collect();
            return Err(format!(
                "{event} has no Claude Code event, so `dialect = \"claude\"` can't bind it (it binds {})",
                has.join(", ")
            ));
        }
        if let Some(pattern) = &self.matcher {
            if !TOOL_EVENTS.contains(&event) {
                return Err(format!(
                    "`matcher` picks tools, on tool.before and tool.after only: {event} is not about a tool call"
                ));
            }
            // haest's pattern: a regular expression (Claude Code's matchers
            // are) or a list would match no tool, and a guard that never
            // fires is worse than an error.
            match check_pattern(pattern, PatternOf::Tools) {
                Ok(()) => {}
                Err(BadPattern::Empty) => {
                    return Err("`matcher` is empty: leave it out for every tool".into());
                }
                Err(BadPattern::List) => {
                    return Err(format!(
                        "`matcher = \"{pattern}\"` is a list, and a matcher is one tool's name or one prefix: no tool's name holds a `,` or a space (`Edit,Write` is two entries)"
                    ));
                }
                // Not a pattern, and a refusal newer than this match.
                Err(_) => {
                    return Err(format!(
                        "`matcher = \"{pattern}\"` is not haest's pattern: a tool's name (`Bash`) or a prefix ending in `*` (`mcp__github__*`); Claude Code's regular expressions are not read (`Edit|Write` is two entries)"
                    ));
                }
            }
        }
        if self.timeout.is_some_and(|t| t.is_zero()) {
            return Err(TIMEOUT.into());
        }
        if !self.headers.is_empty() && matches!(self.target, Target::Command(_)) {
            return Err("`headers` are for a `url` hook only".into());
        }
        for (name, _) in &self.headers {
            let token = |c: char| c.is_ascii_graphic() && c != ':';
            if name.is_empty() || !name.chars().all(token) {
                return Err(format!("`{name}` is not a header name"));
            }
        }
        Ok(())
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
fn check_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Ok(())
    } else {
        Err("`url` must start with http:// or https://".into())
    }
}

/// What a `timeout` that is not one is refused with.
const TIMEOUT: &str = "`timeout` must be a number of seconds above 0 (10, 2.5)";

/// `timeout`: seconds, a whole number or not, read by the one rule every
/// time setting's number is (haest-core's [`duration_in`]) but in seconds,
/// Claude Code's unit for its key; [`HookEntry::check`] refuses 0.
fn timeout(seconds: &Value) -> Result<Duration, String> {
    let n = seconds.as_f64().ok_or(TIMEOUT)?;
    duration_in(n, TimeUnit::Seconds).map_err(|why| match why {
        BadDuration::TooLong { .. } => format!("`timeout`: {why}"),
        _below_zero => TIMEOUT.to_string(),
    })
}

/// `headers`: a table of names to text. An error names the header, never
/// its value.
fn read_headers(headers: &Value) -> Result<Vec<(String, String)>, String> {
    let Value::Object(headers) = headers else {
        return Err("`headers` must be a table: headers = { name = \"value\" }".into());
    };
    let mut read = Vec::new();
    for (name, value) in headers {
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
            map.serialize_entry("timeout", &DurationIn(timeout, TimeUnit::Seconds))?;
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
