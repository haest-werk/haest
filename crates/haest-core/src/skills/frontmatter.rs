//! The two frontmatter readers a SKILL.md needs: strict YAML, and the flat
//! `key: value` line reader ([`crate::frontmatter`]) that takes over when YAML
//! refuses.

use std::collections::{BTreeMap, HashMap};

use yaml_rust2::{
    Event, Yaml, YamlLoader,
    parser::{EventReceiver, Parser},
};

use crate::frontmatter::{block_end, inline_list, unquote};

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
        let parsed = match inline_list(value) {
            Some(items) => Meta::List(items),
            None => Meta::Str(unquote(value)),
        };
        meta.insert(key.to_string(), parsed);
    }
    meta
}

#[cfg(test)]
mod tests;
