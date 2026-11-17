//! One raw value checked against its setting's kind, into the one spelling the
//! typed settings read: `true` / `false`, a comma list (`none` and an empty
//! list are ""), a number, a word. Garbage is refused, never read as a default.

use std::{
    collections::HashMap,
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use haest_core::{__private::is_var_name, fs::resolve_path};

use crate::{
    fields::{up_to, whole_up_to},
    registry::{Def, Kind},
};

/// The variable `key`'s value in `env`, trimmed; None when it is unset or
/// empty. How the loader reads every variable, for a host to read its own the
/// same way (`HOME`).
///
/// ```
/// use std::collections::HashMap;
///
/// let env = HashMap::from([
///     ("HOME".to_string(), " /home/me ".to_string()),
///     ("HAEST_MODEL".to_string(), String::new()),
/// ]);
/// assert_eq!(haest_config::env_var(&env, "HOME"), Some("/home/me"));
/// assert_eq!(haest_config::env_var(&env, "HAEST_MODEL"), None);
/// ```
pub fn env_var<'a, S: BuildHasher>(
    env: &'a HashMap<String, String, S>,
    key: &str,
) -> Option<&'a str> {
    env.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// `~/x` -> home/x, `x` -> cwd/x, normalized: every path a setting names.
pub(crate) fn absolute(dir: &str, cwd: &Path, home: &Path) -> PathBuf {
    match dir.strip_prefix("~/") {
        Some(rest) => resolve_path(home, rest),
        None if dir == "~" => resolve_path(home, "."),
        None => resolve_path(cwd, dir),
    }
}

/// true / 1 / on / yes and false / 0 / off / no, any case.
pub(crate) fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" => Some(true),
        "false" | "0" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// `a, b,,c` -> [a, b, c].
pub(crate) fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// A number >= 0; with `whole_up_to`, a whole one no larger (a count: a
/// fraction would be cut to a smaller count, and 0 is "no cap").
pub(crate) fn number(raw: &str, whole_up_to: Option<f64>) -> Result<f64, String> {
    let n = raw
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0);
    match (n, whole_up_to) {
        (None, _) => Err(format!("\"{raw}\" must be a number >= 0")),
        (Some(n), Some(max)) if n.fract() != 0.0 || n > max => {
            Err(format!("\"{raw}\" must be a whole number from 0 to {max}"))
        }
        (Some(n), _) => Ok(n),
    }
}

/// `raw` as `def` takes it, or why not.
pub(crate) fn check(def: &Def, raw: &str) -> Result<String, String> {
    let word = raw.trim();
    match def.kind {
        Kind::Bool => parse_bool(word)
            .map(|b| b.to_string())
            .ok_or_else(|| format!("\"{word}\" must be true or false")),
        Kind::Number => match (number(word, whole_up_to(def.key))?, up_to(def.key)) {
            (n, Some(max)) if n > max => {
                Err(format!("\"{word}\" must be a number from 0 to {max}"))
            }
            _ => Ok(word.to_string()),
        },
        Kind::Text | Kind::Path => Ok(word.to_string()),
        Kind::Choice(words) => words
            .iter()
            .find(|w| w.eq_ignore_ascii_case(word))
            .map(|w| w.to_string())
            .ok_or_else(|| format!("\"{word}\" must be one of {}", words.join(", "))),
        Kind::List(allowed) => list(word, allowed),
        // A variable's value keeps its spaces.
        Kind::Vars => var(raw),
    }
}

/// One `NAME=VALUE`; a bare `NAME` is haest's own value (`NAME=${NAME}`). An
/// error names the name only: the value may be a key.
pub(crate) fn var(raw: &str) -> Result<String, String> {
    match raw.split_once('=') {
        Some((name, value)) => named_var(name.trim(), value),
        None => named_var(raw.trim(), &format!("${{{}}}", raw.trim())),
    }
}

/// `name=value` checked: `name` must be a variable name (a TOML key may hold an `=`).
pub(crate) fn named_var(name: &str, value: &str) -> Result<String, String> {
    if !is_var_name(name) {
        return Err(format!(
            "\"{name}\" is not a variable name (NAME=VALUE: a letter or _, then letters, digits and _)"
        ));
    }
    Ok(format!("{name}={value}"))
}

/// A checked `NAME=VALUE`, as its name and its value.
pub(crate) fn var_parts(checked: &str) -> (&str, &str) {
    checked.split_once('=').unwrap_or((checked, ""))
}

fn list(raw: &str, allowed: Option<&[&str]>) -> Result<String, String> {
    let items = split_list(raw);
    let Some(words) = allowed else {
        return Ok(match items.as_slice() {
            [one] if one == "none" => String::new(),
            _ => items.join(","),
        });
    };
    let mut out: Vec<&str> = Vec::new();
    for item in &items {
        match item.to_ascii_lowercase().as_str() {
            "none" => {}
            "all" => out.extend(words.iter()),
            w => match words.iter().find(|a| **a == w) {
                Some(a) => out.push(a),
                None => {
                    return Err(format!(
                        "\"{item}\" is not one of {} (or all, none)",
                        words.join(", ")
                    ));
                }
            },
        }
    }
    // Registry order, once each: `bash,read,read` and `read,bash` mean the same.
    let picked: Vec<&str> = words.iter().copied().filter(|w| out.contains(w)).collect();
    Ok(picked.join(","))
}
