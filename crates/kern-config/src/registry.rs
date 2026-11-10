//! Every setting kern has, once. A setting has one name written three ways:
//! `skill_catalog` in a TOML file, `KERN_SKILL_CATALOG` in the environment,
//! `--skill-catalog` on the command line. Its default is the bare value
//! (`Config::bare`): every capability off until asked for, every brake on.

use kern_settings::{BuiltinTools, ContextSources, SkillSources, WordSet};

pub use kern_settings::DEFAULT_MODEL;

const WEB_SEARCH: &[&str] = &[
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
    /// A dir: `~/x` is in the home dir, `x` in the working dir.
    Path,
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
        flag_name(self.key)
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

const fn def(key: &'static str, group: &'static str, kind: Kind, help: &'static str) -> Def {
    Def {
        key,
        group,
        kind,
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

use Kind::{Bool, Choice, List, Number, Path, Text};

/// One row per setting, in the order `--help` and `kern settings` list them.
#[rustfmt::skip]
pub const DEFS: &[Def] = &[
    def("profile", PROFILE, Text, "A named set of settings: claude (built in: everything Claude Code has that kern reads), or a [profiles.<name>] table in a config file"),
    def("model", MODEL, Text, "provider:model").short('m'),
    def("effort", MODEL, Choice(&["off", "low", "medium", "high", "xhigh", "max"]), "Reasoning effort (unset: the model's own default)").short('e'),
    def("system_prompt", MODEL, Text, "Replaces kern's own identity text at the top of the system prompt"),
    def("append_system_prompt", MODEL, Text, "Text appended to the system prompt"),
    def("context", CONTEXT, List(Some(ContextSources::WORDS)), "Instruction files: agents (AGENTS.md: ~/.agents/AGENTS.md, then every dir down to here), claude (CLAUDE.md, .claude/CLAUDE.md, CLAUDE.local.md, ~/.claude/CLAUDE.md)"),
    def("skills", CONTEXT, List(Some(SkillSources::WORDS)), "Where skills come from: agents (~/.agents/skills, .agents/skills), claude (~/.claude/skills, .claude/skills), plugins (Claude Code's installed plugins); /<name> runs one"),
    def("skill_catalog", CONTEXT, Bool, "List the skills in the system prompt, so the model picks them on its own"),
    def("tools", TOOLS, List(Some(BuiltinTools::WORDS)), "Built-in tools: read, write, edit, bash, grep, glob, web_fetch, tasks, skill (all = every one)"),
    def("code", TOOLS, Bool, "The Code tool: a JavaScript run that calls the other tools"),
    def("code_tools", TOOLS, List(None), "Extra tool names Code may call (exact, or prefix*)"),
    def("agents", TOOLS, Bool, "Agent, SendMessage and StopAgent: subagents"),
    def("web_search", TOOLS, Choice(WEB_SEARCH), "The provider's web search, by engine"),
    def("image", TOOLS, Bool, "GenerateImage, over OpenRouter (needs OPENROUTER_API_KEY)"),
    def("image_model", TOOLS, Text, "GenerateImage's default model"),
    def("image_models", TOOLS, List(None), "The other image models a call may pick (unset: the library's list)"),
    def("mcp", TOOLS, Bool, "Start the project's MCP servers (.mcp.json); a trusted dir only"),
    def("deferred_tools", TOOLS, Choice(&["proxy", "grow", "off"]), "How MCP tools reach the model: behind ToolSearch (proxy, grow) or all up front (off)"),
    def("workspace", WORK, Choice(&["sandbox", "real"]), "sandbox: a simulated bash over a copy-on-write overlay (nothing reaches the disk until /apply); real: the real disk and bash").short('w'),
    def("env_passthrough", WORK, Choice(&["none", "core", "all"]), "What a process kern starts (bash, a background job, an MCP server) gets of kern's environment: none, core (PATH, HOME, the user, the locale, the temp dirs), all (every variable, API keys included)"),
    def("mods", WORK, Bool, "Your mods in <config dir>/mods"),
    def("project_mods", WORK, Bool, "The project's mods in .kern/mods (its code runs; a trusted dir only)"),
    def("approvals", WORK, Bool, "The built-in approval mod: asks before a tool changes anything"),
    def("trusted_dirs", WORK, List(None), "Dirs whose project files kern reads (.kern/config.toml, .mcp.json, .kern/mods), with every dir inside them").scope(Scope::User),
    def("max_turn_usd", LIMITS, Number, "One turn's spend (0 = no cap)"),
    def("max_turn_tokens", LIMITS, Number, "One turn's tokens (unset: 10x the model's window; 0 = no cap)"),
    def("max_request_tokens", LIMITS, Number, "One model call's input tokens (unset: 90% of the window; 0 = no cap)"),
    def("max_turn_steps", LIMITS, Number, "Model calls in one turn (0 = no cap)"),
    def("max_turn_minutes", LIMITS, Number, "One turn's wall clock (0 = no cap)"),
    def("max_repeats", LIMITS, Number, "The same tool call in a row before the guard stops it (0 = no cap)"),
    def("max_conversation_usd", LIMITS, Number, "The whole conversation's spend (0 = no cap)"),
    def("max_conversation_tokens", LIMITS, Number, "Tokens of models with no price (0 = no cap)"),
    def("context_window", LIMITS, Number, "The model's window in tokens (unset: the catalog's, else 200000)"),
    def("compact_at", LIMITS, Number, "Compact the conversation past this many tokens (0 = at 80% of the window only)"),
    def("max_agent_turns", AGENTS, Number, "Turns in a row a job or an agent starts with no human message; past it kern holds them (0 = no cap)"),
    def("max_agent_depth", AGENTS, Number, "How deep agents nest, main = 0 (0 = no Agent tool)"),
    def("max_subagents", AGENTS, Number, "Subagents running at once, across the tree (0 = no Agent tool)"),
    def("subagent_usd", AGENTS, Number, "One subagent's spend"),
    def("subagent_steps", AGENTS, Number, "One subagent's model calls"),
    def("subagent_minutes", AGENTS, Number, "One subagent's wall clock"),
    def("data_dir", PATHS, Path, "Sessions, history and spill files (unset: $XDG_DATA_HOME/kern, else ~/.local/share/kern)"),
    def("config_dir", PATHS, Path, "config.toml, providers and mods (unset: $XDG_CONFIG_HOME/kern, else ~/.config/kern)").scope(Scope::Process),
    def("debug", PATHS, Bool, "Log what kern does under the hood"),
];

pub fn find(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

/// A setting's flag, without the dashes in front: `skill_catalog` -> `skill-catalog`.
pub fn flag_name(key: &str) -> String {
    key.replace('_', "-")
}
