//! Every setting kern has, once. A setting has one name written three ways:
//! `skill_catalog` in a TOML file, `KERN_SKILL_CATALOG` in the environment,
//! `--skill-catalog` on the command line. The defaults are the BARE kern:
//! every capability off until asked for, every brake on.

use kern_tools::image::DEFAULT_IMAGE_MODEL;

/// The model when none is configured.
pub const DEFAULT_MODEL: &str = "openrouter:anthropic/claude-sonnet-5.5";

/// Instruction files: `agents` = AGENTS.md, `claude` = Claude Code's CLAUDE.md family.
pub const CONTEXT_SOURCES: &[&str] = &["agents", "claude"];
/// Skill dirs: `agents` = .agents/skills, `claude` = .claude/skills, `plugins` = Claude Code's plugins.
pub const SKILL_SOURCES: &[&str] = &["agents", "claude", "plugins"];
/// The built-in tools `tools` picks from.
pub const BUILTIN_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "bash",
    "grep",
    "glob",
    "web_fetch",
    "tasks",
    "skill",
];
pub const WEB_SEARCH: &[&str] = &[
    "off",
    "auto",
    "native",
    "exa",
    "parallel",
    "firecrawl",
    "perplexity",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// true / false (also 1 / 0, on / off, yes / no).
    Bool,
    /// A number >= 0.
    Number,
    Text,
    /// One of these words.
    Choice(&'static [&'static str]),
    /// A comma list, or a TOML array. `Some` = only these words, plus `all`
    /// (every one of them) and `none` (an empty list).
    List(Option<&'static [&'static str]>),
}

/// Where a setting may come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Any file, any profile, the environment and flags.
    Any,
    /// The human's own: never from a project's file (a repo must not trust itself).
    User,
    /// The environment and flags only: it says where the files are.
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Def {
    /// The TOML name; the flag and the variable are spelled from it.
    pub key: &'static str,
    /// The heading it is listed under.
    pub group: &'static str,
    pub kind: Kind,
    /// The bare default as it would be written; None = computed (the help says how).
    pub default: Option<&'static str>,
    pub short: Option<char>,
    pub scope: Scope,
    pub help: &'static str,
}

impl Def {
    /// `KERN_SKILL_CATALOG`.
    pub fn env(&self) -> String {
        format!("KERN_{}", self.key.to_ascii_uppercase())
    }

    /// `skill-catalog` (without the dashes in front).
    pub fn flag(&self) -> String {
        self.key.replace('_', "-")
    }

    const fn short(mut self, c: char) -> Self {
        self.short = Some(c);
        self
    }

    const fn scope(mut self, s: Scope) -> Self {
        self.scope = s;
        self
    }
}

const fn def(
    key: &'static str,
    group: &'static str,
    kind: Kind,
    default: Option<&'static str>,
    help: &'static str,
) -> Def {
    Def {
        key,
        group,
        kind,
        default,
        short: None,
        scope: Scope::Any,
        help,
    }
}

const PROFILE: &str = "Profile";
const MODEL: &str = "Model and prompt";
const CONTEXT: &str = "Context";
const TOOLS: &str = "Tools";
const WORK: &str = "Workspace, mods and trust";
const LIMITS: &str = "Limits";
const AGENTS: &str = "Subagents";
const PATHS: &str = "Paths";

use Kind::{Bool, Choice, List, Number, Text};

/// One row per setting, in the order `--help` and `kern settings` list them.
#[rustfmt::skip]
pub const DEFS: &[Def] = &[
    def("profile", PROFILE, Text, None, "A named set of settings: claude (built in: everything Claude Code has that kern reads), or a [profiles.<name>] table in a config file"),
    def("model", MODEL, Text, Some(DEFAULT_MODEL), "provider:model").short('m'),
    def("effort", MODEL, Choice(&["off", "low", "medium", "high", "xhigh", "max"]), None, "Reasoning effort (unset: the model's own default)").short('e'),
    def("system_prompt", MODEL, Text, None, "Replaces kern's own identity text at the top of the system prompt"),
    def("append_system_prompt", MODEL, Text, None, "Text appended to the system prompt"),
    def("context", CONTEXT, List(Some(CONTEXT_SOURCES)), Some("none"), "Instruction files: agents (AGENTS.md: ~/.agents/AGENTS.md, then every dir down to here), claude (CLAUDE.md, .claude/CLAUDE.md, CLAUDE.local.md, ~/.claude/CLAUDE.md)"),
    def("skills", CONTEXT, List(Some(SKILL_SOURCES)), Some("none"), "Where skills come from: agents (~/.agents/skills, .agents/skills), claude (~/.claude/skills, .claude/skills), plugins (Claude Code's installed plugins); /<name> runs one"),
    def("skill_catalog", CONTEXT, Bool, Some("false"), "List the skills in the system prompt, so the model picks them on its own"),
    def("tools", TOOLS, List(Some(BUILTIN_TOOLS)), Some("none"), "Built-in tools: read, write, edit, bash, grep, glob, web_fetch, tasks, skill (all = every one)"),
    def("code", TOOLS, Bool, Some("false"), "The Code tool: a JavaScript run that calls the other tools"),
    def("code_tools", TOOLS, List(None), Some("none"), "Extra tool names Code may call (exact, or prefix*)"),
    def("agents", TOOLS, Bool, Some("false"), "Agent, SendMessage and StopAgent: subagents"),
    def("web_search", TOOLS, Choice(WEB_SEARCH), Some("off"), "The provider's web search, by engine"),
    def("image", TOOLS, Bool, Some("false"), "GenerateImage, over OpenRouter (needs OPENROUTER_API_KEY)"),
    def("image_model", TOOLS, Text, Some(DEFAULT_IMAGE_MODEL), "GenerateImage's default model"),
    def("image_models", TOOLS, List(None), None, "The other image models a call may pick (unset: the library's list)"),
    def("mcp", TOOLS, Bool, Some("false"), "The project's MCP servers (.mcp.json), started; a trusted dir only"),
    def("deferred_tools", TOOLS, Choice(&["proxy", "grow", "off"]), Some("proxy"), "How MCP tools reach the model: behind ToolSearch (proxy, grow) or all up front (off)"),
    def("workspace", WORK, Choice(&["sandbox", "real"]), Some("sandbox"), "sandbox: a simulated bash over a copy-on-write overlay (nothing reaches the disk until /apply); real: the real disk and bash").short('w'),
    def("mods", WORK, Bool, Some("false"), "Your mods in <config dir>/mods"),
    def("project_mods", WORK, Bool, Some("false"), "The project's mods in .kern/mods (its code runs; a trusted dir only)"),
    def("approvals", WORK, Bool, Some("true"), "The built-in approval mod: asks before a tool changes anything"),
    def("trusted_dirs", WORK, List(None), Some("none"), "Dirs whose project files kern reads (.kern/config.toml, .mcp.json, .kern/mods), with every dir inside them").scope(Scope::User),
    def("max_turn_usd", LIMITS, Number, Some("2"), "One turn's spend (0 = no cap)"),
    def("max_turn_tokens", LIMITS, Number, None, "One turn's tokens (unset: 10x the model's window; 0 = no cap)"),
    def("max_request_tokens", LIMITS, Number, None, "One model call's input tokens (unset: 90% of the window; 0 = no cap)"),
    def("max_turn_steps", LIMITS, Number, Some("100"), "Model calls in one turn (0 = no cap)"),
    def("max_turn_minutes", LIMITS, Number, Some("60"), "One turn's wall clock (0 = no cap)"),
    def("max_repeats", LIMITS, Number, Some("6"), "The same tool call in a row before the guard stops it (0 = no cap)"),
    def("max_conversation_usd", LIMITS, Number, Some("20"), "The whole conversation's spend (0 = no cap)"),
    def("max_conversation_tokens", LIMITS, Number, Some("20000000"), "Tokens of models with no price (0 = no cap)"),
    def("context_window", LIMITS, Number, None, "The model's window in tokens (unset: the catalog's, else 200000)"),
    def("compact_at", LIMITS, Number, Some("400000"), "Compact the conversation past this many tokens (0 = at 80% of the window only)"),
    def("max_agent_turns", AGENTS, Number, Some("30"), "Turns in a row a job or an agent starts with no human message; past it kern holds them (0 = no cap)"),
    def("max_agent_depth", AGENTS, Number, Some("2"), "How deep agents nest, main = 0 (0 = no Agent tool)"),
    def("max_subagents", AGENTS, Number, Some("4"), "Subagents running at once, across the tree (0 = no Agent tool)"),
    def("subagent_usd", AGENTS, Number, Some("1"), "One subagent's spend"),
    def("subagent_steps", AGENTS, Number, Some("50"), "One subagent's model calls"),
    def("subagent_minutes", AGENTS, Number, Some("15"), "One subagent's wall clock"),
    def("data_dir", PATHS, Text, None, "Sessions, history and spill files (unset: $XDG_DATA_HOME/kern, else ~/.local/share/kern)"),
    def("config_dir", PATHS, Text, None, "config.toml, providers and mods (unset: $XDG_CONFIG_HOME/kern, else ~/.config/kern)").scope(Scope::Process),
    def("debug", PATHS, Bool, Some("false"), "Log what kern does under the hood"),
];

pub fn find(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}
