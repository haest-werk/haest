//! The two frontmatter readers a SKILL.md needs: strict YAML, and the flat
//! `key: value` line reader that takes over when YAML refuses.

use std::collections::{BTreeMap, HashMap};

use yaml_rust2::{
    Event, Yaml, YamlLoader,
    parser::{EventReceiver, Parser},
};

/// One frontmatter value, as the skill reader asks about it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Meta {
    Str(String),
    Bool(bool),
    List(Vec<String>),
    /// A number, a mapping, null: nothing a skill field reads.
    Other,
}

pub(super) type MetaMap = BTreeMap<String, Meta>;

/// `---\n<head>\n---\n<body>` -> (head lines, body), or None when there is no block.
/// The head ends at the FIRST `\n---` after the opening fence.
pub(super) fn split_frontmatter(content: &str) -> Option<(Vec<&str>, &str)> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let after = &rest[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    Some((rest[..end].split('\n').collect(), body))
}

/// What expanding its aliases may add to a frontmatter, in [`ExpandedSize`]
/// units. yaml-rust2 expands an alias by copying its anchor, so nine levels of
/// `&a [*a, *a, ...]` (a few hundred bytes) would build ~9^9 nodes.
const ALIAS_ROOM: u64 = 64 * 1024;

/// The document's size with every alias expanded, counted from the parser's
/// events alone (nothing is built): a node is 1, a scalar also its bytes, an
/// alias the size of its anchor.
#[derive(Default)]
struct ExpandedSize {
    /// The collections still open: their anchor id (0 = none) and size so far.
    open: Vec<(usize, u64)>,
    anchors: HashMap<usize, u64>,
    total: u64,
}

impl EventReceiver for ExpandedSize {
    fn on_event(&mut self, ev: Event) {
        let (anchor, size) = match ev {
            Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _) => {
                self.open.push((anchor, 1));
                return;
            }
            Event::SequenceEnd | Event::MappingEnd => match self.open.pop() {
                Some(done) => done,
                None => return,
            },
            Event::Scalar(text, _, anchor, _) => (anchor, 1 + text.len() as u64),
            Event::Alias(id) => (0, self.anchors.get(&id).copied().unwrap_or(1)),
            _ => return,
        };
        if anchor > 0 {
            self.anchors.insert(anchor, size);
        }
        let into = match self.open.last_mut() {
            Some((_, parent)) => parent,
            None => &mut self.total,
        };
        *into = into.saturating_add(size);
    }
}

/// Strict YAML first (block scalars, lists); the flat line reader when that
/// refuses. Hand-written skills routinely carry an unquoted `description: ... e.g:
/// "x"` -- invalid YAML that Claude Code loads anyway, so refusing it would hide
/// a skill the model can see. A frontmatter whose aliases would expand past
/// [`ALIAS_ROOM`] is refused whole (None: that skill is skipped), before
/// anything is built: a project's skill must not be able to exhaust memory.
pub(super) fn read_meta(head: &[&str]) -> Option<MetaMap> {
    let text = head.join("\n");
    let mut size = ExpandedSize::default();
    let parsed = Parser::new(text.chars()).load(&mut size, true);
    if parsed.is_ok() && size.total > text.len() as u64 + ALIAS_ROOM {
        return None;
    }
    match parsed.and_then(|()| YamlLoader::load_from_str(&text)) {
        Ok(docs) => match docs.into_iter().next() {
            Some(Yaml::Hash(h)) => Some(
                h.into_iter()
                    .filter_map(|(k, v)| Some((k.into_string()?, from_yaml(v))))
                    .collect(),
            ),
            _ => None,
        },
        Err(_) => {
            let flat = flat_meta(head);
            (!flat.is_empty()).then_some(flat)
        }
    }
}

fn scalar_string(v: &Yaml) -> String {
    match v {
        Yaml::String(s) | Yaml::Real(s) => s.clone(),
        Yaml::Integer(i) => i.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        Yaml::Null => "null".into(),
        _ => String::new(),
    }
}

fn from_yaml(v: Yaml) -> Meta {
    match v {
        Yaml::String(s) => Meta::Str(s),
        Yaml::Boolean(b) => Meta::Bool(b),
        Yaml::Array(a) => Meta::List(a.iter().map(scalar_string).collect()),
        _ => Meta::Other,
    }
}

/// The flat subset: `key: value` lines, inline `[a, b]` lists, quoted scalars.
/// A key that opens an indented block is skipped whole -- the flat reader cannot
/// represent it, and reading its children as top-level keys would invert it.
fn flat_meta(lines: &[&str]) -> MetaMap {
    let mut meta = MetaMap::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some(idx) = line.find(':') else { continue };
        let key = line[..idx].trim();
        let value = line[idx + 1..].trim();
        if let Some(end) = block_end(lines, i - 1, value) {
            i = end;
            continue;
        }
        let parsed = if value.starts_with('[') && value.ends_with(']') && value.len() >= 2 {
            Meta::List(inline_list(value))
        } else {
            Meta::Str(unquote(value))
        };
        meta.insert(key.to_string(), parsed);
    }
    meta
}

/// Nothing at all (a nested block) or a block-scalar indicator (`|`, `>-`, `|+2`).
fn opens_a_block(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['|', '>']) else {
        return value.is_empty();
    };
    let rest = rest.strip_prefix(['-', '+']).unwrap_or(rest);
    rest.bytes().all(|b| b.is_ascii_digit())
}

/// One past the last indented line under `lines[head]`, or None when it opens no block.
/// A blank line does not end the block; trailing blanks stay out of it.
fn block_end(lines: &[&str], head: usize, value: &str) -> Option<usize> {
    if !opens_a_block(value) {
        return None;
    }
    let mut last = head;
    for (i, line) in lines.iter().enumerate().skip(head + 1) {
        let indented = line.starts_with(char::is_whitespace) && !line.trim().is_empty();
        if indented {
            last = i;
        } else if !line.trim().is_empty() {
            break;
        }
    }
    (last != head).then_some(last + 1)
}

fn inline_list(raw: &str) -> Vec<String> {
    raw[1..raw.len() - 1]
        .split(',')
        .map(|s| unquote(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// A matched pair of quotes at both ends is unwrapped; escapes are undone for a
/// DOUBLE-quoted scalar only (a single-quoted one has none). Anything else stays as is.
pub(super) fn unquote(raw: &str) -> String {
    let mut chars = raw.chars();
    let (Some(q), Some(last)) = (chars.next(), chars.next_back()) else {
        return raw.to_string();
    };
    if (q != '"' && q != '\'') || last != q {
        return raw.to_string();
    }
    let inner = chars.as_str();
    if q == '"' {
        unescape(inner)
    } else {
        inner.to_string()
    }
}

/// The inverse of a double-quoted scalar's escapes, in one pass.
fn unescape(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            out.push('\\');
            break;
        };
        let hex = || {
            let mut probe = chars.clone();
            let h: String = probe.by_ref().take(2).collect();
            (h.len() == 2 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| u8::from_str_radix(&h, 16).ok())
                .flatten()
        };
        match e {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'x' => match hex() {
                Some(b) => {
                    out.push(char::from(b));
                    chars.next();
                    chars.next();
                }
                None => out.push('x'),
            },
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests;
