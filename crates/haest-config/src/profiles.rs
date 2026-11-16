//! Profiles haest ships. A config file's `[profiles.<name>]` table adds to one of
//! these (its keys win) or defines a new one.

use crate::{
    ConfigError,
    source::Source,
    stack::{Given, Layer, Stack},
};

/// `claude`: everything Claude Code has that haest reads. NOT parity: haest does
/// not read Claude Code's agents, commands, hooks, permissions, user or plugin
/// MCP servers, `@` imports, rules, memory or output styles.
const CLAUDE: &[(&str, &str)] = &[
    ("context", "agents,claude"),
    ("skills", "all"),
    ("skill_catalog", "true"),
    ("tools", "all"),
    ("agents", "true"),
    ("web_search", "native"),
    ("mcp", "true"),
    ("workspace", "real"),
    // Claude Code hands its whole environment to what it starts.
    ("env_passthrough", "all"),
];

pub const BUILTIN: &[(&str, &[(&str, &str)])] = &[("claude", CLAUDE)];

pub fn builtin(name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    BUILTIN.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}

/// The profile `name`: the built-in one, then each layer's table of it on top.
pub(crate) fn layer(name: &str, stack: &Stack) -> Result<Layer, ConfigError> {
    let builtin = builtin(name);
    let tables = stack.profile_tables(name);
    if builtin.is_none() && tables.is_empty() {
        // Each once: a file's table may add to a built-in one, or to another file's.
        let mut known: Vec<String> = Vec::new();
        for n in BUILTIN.iter().map(|(n, _)| *n).chain(stack.profile_names()) {
            if !known.iter().any(|k| k == n) {
                known.push(n.into());
            }
        }
        return Err(ConfigError::UnknownProfile {
            name: name.into(),
            known,
        });
    }
    let source = Source::Profile(name.to_string());
    let given = builtin
        .unwrap_or_default()
        .iter()
        .map(|(key, raw)| Given::new(*key, *raw, source.clone()))
        .collect();
    let mut layer = Layer::checked(given)?;
    let tables = tables.into_iter().flatten();
    layer
        .values
        .extend(tables.map(|(def, value)| (*def, value.clone(), source.clone())));
    Ok(layer)
}
