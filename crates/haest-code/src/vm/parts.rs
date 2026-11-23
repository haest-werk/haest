//! The small pieces of one engine run ([`super::engine`]): console buffer, base
//! prelude, pure helpers.

use std::{ops::Add, time::Duration};

use rquickjs::{Coerced, Value};

/// The furthest ahead a run's deadline lies: thirty years, as far as tokio's
/// own timers reach. A longer timeout is no deadline at all.
const FAR_FUTURE: Duration = Duration::from_secs(30 * 365 * 24 * 60 * 60);

/// `at` plus `by`, `by` held to [`FAR_FUTURE`]: every deadline of a run, std's
/// or tokio's `Instant`, so no timeout a host hands in can overflow one
/// (`Duration::MAX` included), nor can a few milliseconds added to it.
pub(crate) fn later<I: Add<Duration, Output = I>>(at: I, by: Duration) -> I {
    at + by.min(FAR_FUTURE)
}

/// On the same line as the code's first line, so a stack's line numbers are the model's own.
pub(crate) fn as_function(code: &str) -> String {
    format!(
        "(async () => {{{code}\n}})().then(__settle).then(v => v === undefined ? undefined : typeof v === 'string' ? v : JSON.stringify(v, null, 2))"
    )
}

/// What a run out of memory says: QuickJS's own error, which it throws when
/// it has the room to build it.
const OUT_OF_MEMORY: &str = "InternalError: out of memory";

/// `Name: message (line N)` -- the line in the model's own code, the way Claude
/// Code's REPL reports it. A thrown non-object is its string form, except
/// `null`: QuickJS throws `null` when it runs out of memory and has no room
/// left to build the error (`JS_ThrowError2`), so `null` reads as
/// [`OUT_OF_MEMORY`]. A guest's own `throw null` reads the same; nothing
/// tells the two apart once the run has failed.
pub(crate) fn error_text(e: &Value<'_>, filename: &str) -> String {
    if e.is_null() {
        return OUT_OF_MEMORY.into();
    }
    let Some(obj) = e.as_object() else {
        return e
            .get::<Coerced<String>>()
            .map(|s| s.0)
            .unwrap_or_else(|_| "Error".into());
    };
    let prop = |key: &str| {
        obj.get::<_, Option<Coerced<String>>>(key)
            .ok()
            .flatten()
            .map(|s| s.0)
    };
    let line = prop("stack")
        .and_then(|s| line_in(&s, filename))
        .map(|n| format!(" (line {n})"))
        .unwrap_or_default();
    format!(
        "{}: {}{line}",
        prop("name").unwrap_or_else(|| "Error".into()),
        prop("message").unwrap_or_default()
    )
}

/// The line number after the first `<filename>:` in a stack.
fn line_in(stack: &str, filename: &str) -> Option<String> {
    let needle = format!("{filename}:");
    let at = stack.find(&needle)? + needle.len();
    let digits: String = stack[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then_some(digits)
}

/// Console output, capped: past `max` characters the rest is dropped and
/// `truncated` says so. Kept three ways: `text` interleaved with a level tag (what
/// the Code tool shows the model), `stdout` / `stderr` split (what the sandbox `js`
/// command prints). Counted in characters, never splitting one.
#[derive(Debug, Default)]
pub(crate) struct Logs {
    max: usize,
    pub text: String,
    text_chars: usize,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

impl Logs {
    pub(super) fn new(max: usize) -> Self {
        Self {
            max,
            ..Default::default()
        }
    }

    pub(super) fn add(&mut self, level: &str, line: &str) {
        if self.truncated {
            return;
        }
        let room = self.max.saturating_sub(self.text_chars);
        let tag = if level == "log" {
            String::new()
        } else {
            format!("[{level}] ")
        };
        let entry = format!("{tag}{line}\n");
        let plain: String = format!("{line}\n").chars().take(room).collect();
        if level == "log" {
            self.stdout.push_str(&plain);
        } else {
            self.stderr.push_str(&plain);
        }
        let len = entry.chars().count();
        if len > room {
            self.text.extend(entry.chars().take(room));
            self.text_chars = self.max;
            self.truncated = true;
        } else {
            self.text.push_str(&entry);
            self.text_chars += len;
        }
    }
}

/// What every guest gets: `console`, and `__settle`, which awaits a value the way a
/// reader expects -- a promise, and every promise inside a plain object or array (a
/// few levels deep). So `return { a: tools.Read(...), b: tools.Grep(...) }` hands
/// back both RESULTS, both calls having run at once, not two empty `{}`.
pub(crate) const BASE_PRELUDE: &str = r#"'use strict';
{
  const fmt = v => { if (typeof v === 'string') return v; try { return JSON.stringify(v) ?? String(v) } catch { return String(v) } };
  const log = level => (...a) => __log(level, a.map(fmt).join(' '));
  globalThis.console = { log: log('log'), info: log('log'), debug: log('log'), warn: log('warn'), error: log('error') };
  const plain = v => v !== null && typeof v === 'object' && (Array.isArray(v) || Object.getPrototypeOf(v) === Object.prototype);
  const settle = async (v, depth) => {
    if (v && typeof v.then === 'function') v = await v;
    if (depth >= 4 || !plain(v)) return v;
    if (Array.isArray(v)) return Promise.all(v.map(x => settle(x, depth + 1)));
    const keys = Object.keys(v);
    const vals = await Promise.all(keys.map(k => settle(v[k], depth + 1)));
    return Object.fromEntries(keys.map((k, i) => [k, vals[i]]));
  };
  globalThis.__settle = v => settle(v, 0);
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_stop_at_the_cap_and_say_so() {
        let mut logs = Logs::new(10);
        logs.add("log", "12345");
        logs.add("warn", "67890");
        assert!(logs.truncated);
        assert_eq!(logs.text.chars().count(), 10);
        // Nothing lands after the cap.
        logs.add("log", "more");
        assert_eq!(logs.stdout, "12345\n");
    }

    #[test]
    fn a_deadline_is_its_timeout_away_up_to_thirty_years() {
        let now = std::time::Instant::now();
        assert_eq!(
            later(now, Duration::from_secs(5)),
            now + Duration::from_secs(5)
        );
        assert_eq!(later(now, Duration::MAX), now + FAR_FUTURE);
        let tokio_now = tokio::time::Instant::now();
        assert_eq!(later(tokio_now, Duration::MAX), tokio_now + FAR_FUTURE);
    }

    #[test]
    fn a_stack_line_is_found_after_its_own_filename() {
        let stack = "    at f (prelude.js:9:1)\n    at <anonymous> (code.js:3:5)\n";
        assert_eq!(line_in(stack, "code.js").as_deref(), Some("3"));
        assert_eq!(line_in(stack, "other.js"), None);
    }
}
