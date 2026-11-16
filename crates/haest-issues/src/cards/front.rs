//! A card file: its frontmatter keys IN ORDER, and its body. Read with haest's
//! flat frontmatter reader ([`haest_core::frontmatter`]); written the way the
//! board's own card store writes (ported from remote-claude's
//! `project-card-file.ts` + `frontmatter.ts`), so a card both write is the same
//! bytes either way.
//!
//! EVERY KEY SURVIVES A WRITE. A key nobody here knows is written back as it
//! was read; a block the flat subset cannot represent (a nested mapping, a
//! block scalar) is kept as its lines, verbatim. The two exceptions are the
//! board's own: a block list under a list key (`refs:` then `  - a`) is written
//! back inline, and the alias keys `blocked_by` / `see_also` are folded into
//! `depends_on` / `relates_to` (one spelling of each fact).

use haest_core::frontmatter::{block_end, inline_list, split_frontmatter, unquote};

/// The keys the store writes first, in this order; every other key follows in
/// the order the card had it, then the verbatim blocks.
const ORDER: [&str; 16] = [
    "title",
    "status",
    "priority",
    "tags",
    "refs",
    "quest",
    "epic",
    "depends_on",
    "relates_to",
    "created",
    "renamed_from",
    "archived_reason",
    "archived_by",
    "delete_at",
    "model",
    "cluster",
];

/// Keys whose value is a list: a block list under one reads as a list.
const LIST_KEYS: [&str; 7] = [
    "tags",
    "refs",
    "depends_on",
    "blocked_by",
    "relates_to",
    "see_also",
    "blocks",
];

/// `(alias, the key it is stored as)`.
const ALIASES: [(&str, &str); 2] = [("blocked_by", "depends_on"), ("see_also", "relates_to")];

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    Str(String),
    List(Vec<String>),
    /// The lines of a block the flat subset cannot represent, its key's line first.
    Block(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Front {
    pub keys: Vec<(String, Val)>,
    pub body: String,
}

impl Front {
    /// None when `content` has no frontmatter block.
    pub fn parse(content: &str) -> Option<Front> {
        let (head, body) = split_frontmatter(content)?;
        let mut front = Front {
            keys: Vec::new(),
            body: body.trim().to_string(),
        };
        let mut i = 0;
        while i < head.len() {
            let line = head[i];
            let Some(colon) = line.find(':') else {
                i += 1;
                continue;
            };
            let key = line[..colon].trim().to_string();
            let value = line[colon + 1..].trim();
            let val = match block_end(&head, i, value) {
                Some(end) => {
                    let block: Vec<&str> = head[i..end].to_vec();
                    i = end;
                    match block_sequence(&block).filter(|_| LIST_KEYS.contains(&key.as_str())) {
                        Some(items) => Val::List(items),
                        None => Val::Block(block.into_iter().map(String::from).collect()),
                    }
                }
                None => {
                    i += 1;
                    match inline_list(value) {
                        Some(items) => Val::List(items),
                        None => Val::Str(unquote(value)),
                    }
                }
            };
            front.set(&key, val);
        }
        for (alias, stored) in ALIASES {
            if front.get(alias).is_some() {
                let mut merged = front.list(stored);
                for item in front.list(alias) {
                    if !merged.contains(&item) {
                        merged.push(item);
                    }
                }
                front.remove(alias);
                if !merged.is_empty() {
                    front.set(stored, Val::List(merged));
                }
            }
        }
        Some(front)
    }

    pub fn get(&self, key: &str) -> Option<&Val> {
        self.keys.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// A scalar key's value; None when absent, blank or not a scalar.
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Val::Str(s)) if !s.trim().is_empty() => Some(s),
            _ => None,
        }
    }

    /// A key's values: a list's items, a scalar as one item, else none.
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Val::List(items)) => items.clone(),
            Some(Val::Str(s)) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    /// Replace the key's value where it stands, or add it last.
    pub fn set(&mut self, key: &str, val: Val) {
        match self.keys.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = val,
            None => self.keys.push((key.to_string(), val)),
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.keys.retain(|(k, _)| k != key);
    }

    /// A list key: set to `items`, or gone when there are none (an empty list
    /// is the same fact as no key).
    pub fn set_list(&mut self, key: &str, items: Vec<String>) {
        if items.is_empty() {
            self.remove(key);
        } else {
            self.set(key, Val::List(items));
        }
    }

    /// The card's bytes: the ordered keys, every other key, the blocks, the body.
    pub fn render(&self) -> String {
        let mut lines = vec!["---".to_string()];
        for key in ORDER {
            match self.get(key) {
                Some(Val::Str(s)) if s.is_empty() => {}
                Some(v @ (Val::Str(_) | Val::List(_))) => lines.push(entry(key, v)),
                _ => {}
            }
        }
        for (key, v) in &self.keys {
            if !ORDER.contains(&key.as_str()) && !matches!(v, Val::Block(_)) {
                lines.push(entry(key, v));
            }
        }
        for (_, v) in &self.keys {
            if let Val::Block(block) = v {
                lines.extend(block.iter().cloned());
            }
        }
        lines.extend(["---".to_string(), String::new(), self.body.clone()]);
        format!("{}\n", lines.join("\n"))
    }
}

fn entry(key: &str, v: &Val) -> String {
    let value = match v {
        Val::Str(s) => scalar(s),
        Val::List(items) => {
            let items: Vec<String> = items.iter().map(|s| scalar(s)).collect();
            format!("[{}]", items.join(", "))
        }
        Val::Block(_) => String::new(),
    };
    format!("{key}: {value}")
}

/// `key:` then `  - item` lines, all at one indent, every item a plain scalar:
/// the items. Any other shape is None and stays a verbatim block.
fn block_sequence(block: &[&str]) -> Option<Vec<String>> {
    let (head, items) = block.split_first()?;
    let (_, after) = head.split_once(':')?;
    if !after.trim().is_empty() || items.is_empty() {
        return None;
    }
    let mut indent: Option<&str> = None;
    let mut out = Vec::new();
    for line in items {
        let trimmed = line.trim_start();
        let pad = &line[..line.len() - trimmed.len()];
        let item = trimmed.strip_prefix('-')?;
        if pad.is_empty() || !item.starts_with(char::is_whitespace) {
            return None;
        }
        let item = item.trim();
        if item.is_empty() || *indent.get_or_insert(pad) != pad || not_a_scalar(item) {
            return None;
        }
        out.push(unquote(item));
    }
    Some(out)
}

/// A mapping entry (`- key: value`) or a nested list (`- - a`); a quoted item never is.
fn not_a_scalar(item: &str) -> bool {
    if item.starts_with(['"', '\'']) {
        return false;
    }
    let nested = (item.strip_prefix('-'))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
    item.contains(": ") || item.ends_with(':') || nested
}

/// A scalar as written: bare when it reads back the same, else double-quoted
/// with its escapes (so a value with a newline still takes one line).
pub(crate) fn scalar(s: &str) -> String {
    let ambiguous = s.is_empty()
        || s != s.trim()
        || s.contains(": ")
        || s.ends_with(':')
        || s.contains(" #")
        || s.starts_with(['[', '{', '"', '\'', '#'])
        || s.chars().any(is_control);
    if !ambiguous {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_control(c) => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_control(c: char) -> bool {
    (c as u32) < 0x20 || c as u32 == 0x7f
}

#[cfg(test)]
mod tests;
