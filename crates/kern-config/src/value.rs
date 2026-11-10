//! One raw value checked against its setting's kind, into the one spelling the
//! typed settings read: `true` / `false`, a comma list (`none` and an empty
//! list are ""), a number, a word. Garbage is refused, never read as a default.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use kern_core::fs::normalize;

use crate::registry::{Def, Kind};

/// A variable's value, trimmed; None when it is unset or empty.
pub(crate) fn env_var<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// `~/x` -> home/x, `x` -> cwd/x, normalized: every path a setting names.
pub(crate) fn absolute(dir: &str, cwd: &Path, home: &Path) -> PathBuf {
    let path = match dir.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if dir == "~" => home.to_path_buf(),
        None => cwd.join(dir),
    };
    normalize(&path)
}

/// true / 1 / on / yes and false / 0 / off / no, any case.
pub fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" => Some(true),
        "false" | "0" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// `a, b,,c` -> [a, b, c].
pub fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// `raw` as `def` takes it, or why not.
pub fn check(def: &Def, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    match def.kind {
        Kind::Bool => parse_bool(raw)
            .map(|b| b.to_string())
            .ok_or_else(|| format!("\"{raw}\" must be true or false")),
        Kind::Number => raw
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|_| raw.to_string())
            .ok_or_else(|| format!("\"{raw}\" must be a number >= 0")),
        Kind::Text | Kind::Path => Ok(raw.to_string()),
        Kind::Choice(words) => words
            .iter()
            .find(|w| w.eq_ignore_ascii_case(raw))
            .map(|w| w.to_string())
            .ok_or_else(|| format!("\"{raw}\" must be one of {}", words.join(", "))),
        Kind::List(allowed) => list(raw, allowed),
    }
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
