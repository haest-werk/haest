//! Profiles kern ships. A config file's `[profiles.<name>]` table adds to one of
//! these (its keys win) or defines a new one.

/// `claude`: everything Claude Code has that kern reads. NOT parity: kern does
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
