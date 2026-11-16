//! Every setting haest has, once. A setting has one name written three ways:
//! `skill_catalog` in a TOML file, `HAEST_SKILL_CATALOG` in the environment,
//! `--skill-catalog` on the command line. Its default is the bare value
//! (`Config::bare`): every capability off until asked for, every brake on.

use haest_core::{Effort, deferred_tools::DeferMode, provider::WebSearchEngine};
use haest_settings::{BuiltinTools, ContextSources, SkillSources, WordSet};

use crate::fields::{BrokerAccess, ControlSocket, EnvPassthrough};

/// `off`, then every engine.
const WEB_SEARCH: [&str; WebSearchEngine::WORDS.len() + 1] = {
    let mut words = ["off"; WebSearchEngine::WORDS.len() + 1];
    let mut i = 0;
    while i < WebSearchEngine::WORDS.len() {
        words[i + 1] = WebSearchEngine::WORDS[i];
        i += 1;
    }
    words
};

/// The workspaces a setting may pick: `shut` (no files, no shell) is for a
/// host that writes its settings out, never a file or flag.
const WORKSPACES: &[&str] = &["sandbox", "real"];

/// What values a setting takes, and how a raw one is checked and spelled.
///
/// Whatever layer a value comes from, it is checked against its kind and
/// written the one way the typed settings read (`On` reads as `true`, `bash,
/// read` as `read,bash`), so a file, a variable and a flag mean the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// true / false (also 1 / 0, on / off, yes / no).
    Bool,
    /// A number >= 0. A setting that counts something (steps, tokens) takes a
    /// whole number only: a fraction is refused, never cut to a smaller count.
    Number,
    /// Any text, as written.
    Text,
    /// A dir: `~/x` is in the home dir, `x` in the working dir.
    Path,
    /// One of these words.
    Choice(&'static [&'static str]),
    /// A comma list, or a TOML array. `Some` = only these words, plus `all`
    /// (every one of them) and `none` (an empty list).
    List(Option<&'static [&'static str]>),
    /// Variables, one `NAME=VALUE` each: a repeated flag, a TOML table. A
    /// value adds to the ones before it; a name set twice keeps the later value.
    Vars,
}

/// Where a setting may come from. A layer that sets one it may not is an
/// error naming the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Any file, any profile, the environment and flags.
    Any,
    /// The human's own: never from a project's file (a repo must not trust itself).
    User,
    /// The environment and flags only: it says where the files are, or how
    /// this one process runs.
    Process,
}

/// One setting: its name, what it takes, where it may come from, and the text
/// `--help` and the settings reference show for it. [`DEFS`] holds them all.
///
/// ```
/// use haest_config::{DEFS, Kind};
///
/// let catalog = DEFS.iter().find(|d| d.key == "skill_catalog").unwrap();
/// assert_eq!(catalog.env(), "HAEST_SKILL_CATALOG");
/// assert_eq!(catalog.flag(), "skill-catalog");
/// assert_eq!(catalog.kind, Kind::Bool);
///
/// let model = DEFS.iter().find(|d| d.key == "model").unwrap();
/// assert_eq!(model.short, Some('m'));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Def {
    /// The TOML name; the flag and the variable are spelled from it.
    pub key: &'static str,
    /// The heading it is listed under.
    pub group: &'static str,
    /// What values it takes.
    pub kind: Kind,
    /// Its one-letter flag (`-m` for `model`), if it has one.
    pub short: Option<char>,
    /// Where it may come from.
    pub scope: Scope,
    /// A value that is not the default, as a flag or a variable holds it:
    /// what the settings reference shows in each spelling.
    pub example: &'static str,
    /// One line on what it does: `--help`'s text.
    pub help: &'static str,
    /// More for the settings reference only, after `help`: what `--help` has
    /// no room for (a host's own API, the fine print).
    pub detail: Option<&'static str>,
}

impl Def {
    /// Its variable: `HAEST_` and the name in upper case (`HAEST_SKILL_CATALOG`).
    pub fn env(&self) -> String {
        format!("HAEST_{}", self.key.to_ascii_uppercase())
    }

    /// Its flag, without the dashes in front: the name with `-` for `_`
    /// (`skill-catalog`).
    pub fn flag(&self) -> String {
        flag_name(self.key)
    }

    /// A `[profiles.<name>]` table may set it: not `profile` itself, nor a
    /// setting that is the human's own or the process's.
    pub fn profile_may_set(&self) -> bool {
        self.key != "profile" && self.scope == Scope::Any
    }

    const fn short(mut self, c: char) -> Self {
        self.short = Some(c);
        self
    }

    const fn scope(mut self, s: Scope) -> Self {
        self.scope = s;
        self
    }

    const fn detail(mut self, text: &'static str) -> Self {
        self.detail = Some(text);
        self
    }
}

const fn def(
    key: &'static str,
    group: &'static str,
    kind: Kind,
    example: &'static str,
    help: &'static str,
) -> Def {
    Def {
        key,
        group,
        kind,
        short: None,
        scope: Scope::Any,
        example,
        help,
        detail: None,
    }
}

const PROFILE: &str = "Profile";
const MODEL: &str = "Model and prompt";
const CONTEXT: &str = "Context";
const TOOLS: &str = "Tools";
const WORK: &str = "Workspace, mods and trust";
const CAPS: &str = "Caps";
const AGENTS: &str = "Subagents";
const SESSION: &str = "Watching and driving";
const PATHS: &str = "Paths";

use Kind::{Bool, Choice, List, Number, Path, Text, Vars};

/// Every setting haest has, one row each, in the order `--help` and
/// `haest settings` list them: key, heading, kind, an example value, help.
///
/// The one list: the loader checks every value against it, a host builds its
/// flags from it, and
/// [docs/settings.md](https://github.com/haest-werk/haest/blob/main/docs/settings.md)
/// is written from it.
#[rustfmt::skip]
pub const DEFS: &[Def] = &[
    def("profile", PROFILE, Text, "claude", "A named set of settings: claude (built in: everything Claude Code has that haest reads), or a [profiles.<name>] table in a config file"),
    def("model", MODEL, Text, "anthropic:claude-opus-5-5", "provider:model").short('m'),
    def("effort", MODEL, Choice(Effort::WORDS), "high", "Reasoning effort (unset: the model's own default)").short('e'),
    def("system_prompt", MODEL, Text, "You are a terse code reviewer.", "Replaces haest's own identity text at the top of the system prompt"),
    def("append_system_prompt", MODEL, Text, "Answer in British English.", "Text appended to the system prompt"),
    def("provider_routing", MODEL, Text, r#"{"global": {"banned": ["novita"]}}"#, "OpenRouter's provider routing, as JSON: {\"global\": {\"banned\": [...], \"favorites\": [...]}, \"overrides\": ..., \"project\": ...}").scope(Scope::User),
    def("context", CONTEXT, List(Some(ContextSources::WORDS)), "agents", "Instruction files: agents (AGENTS.md: ~/.agents/AGENTS.md, then every dir down to here), claude (CLAUDE.md, .claude/CLAUDE.md, CLAUDE.local.md, ~/.claude/CLAUDE.md)"),
    def("skills", CONTEXT, List(Some(SkillSources::WORDS)), "agents,claude", "Where skills come from: agents (~/.agents/skills, .agents/skills), claude (~/.claude/skills, .claude/skills), plugins (Claude Code's installed plugins); /<name> runs one"),
    def("skill_catalog", CONTEXT, Bool, "true", "List the skills in the system prompt, so the model picks them on its own"),
    def("tools", TOOLS, List(Some(BuiltinTools::WORDS)), "read,grep,glob,bash", "Built-in tools: read, write, edit, bash, grep, glob, web_fetch, tasks, skill (all = every one)"),
    def("code", TOOLS, Bool, "true", "The Code tool: a JavaScript run that calls the other tools"),
    def("code_tools", TOOLS, List(None), "Bash,mcp__github__*", "Extra tool names Code may call (exact, or prefix*)"),
    def("agents", TOOLS, Bool, "true", "Agent, SendMessage and StopAgent: subagents"),
    def("web_search", TOOLS, Choice(&WEB_SEARCH), "native", "The provider's web search, by engine"),
    def("image", TOOLS, Bool, "true", "GenerateImage, over OpenRouter (needs OPENROUTER_API_KEY)"),
    def("image_model", TOOLS, Text, "black-forest-labs/flux-3-image", "GenerateImage's default model"),
    def("image_models", TOOLS, List(None), "openai/gpt-image-2.6-sunburst,black-forest-labs/flux-3-image", "The other image models a call may pick"),
    def("mcp", TOOLS, Bool, "true", "Start the project's MCP servers (.mcp.json); a trusted dir only"),
    def("deferred_tools", TOOLS, Choice(DeferMode::WORDS), "grow", "How MCP tools reach the model: behind ToolSearch (proxy, grow) or all up front (off)")
        .detail("ToolSearch lists each held-back tool with its description's first sentence. A host linking haest can hold back its own tools too, all but a list it names (`ToolDeferral::deferring_all_except`, `DeferredTools`); a held-back tool the model calls by name still runs, and loads."),
    def("workspace", WORK, Choice(WORKSPACES), "real", "sandbox: a simulated bash over a copy-on-write overlay (nothing reaches the disk until /apply); real: the real disk and bash").short('w'),
    def("env_passthrough", WORK, Choice(EnvPassthrough::WORDS), "none", "What a process haest starts (bash, a background job, an MCP server) gets of haest's environment: none, core (PATH, HOME, the user, the locale, the temp dirs), all (every variable, API keys included)"),
    def("env", WORK, Vars, "GH_TOKEN=${GITHUB_TOKEN}", "A variable every process haest starts gets, the sandbox's shell included, over env_passthrough: NAME=VALUE, or NAME for haest's own value; repeat it for more. In a file: an [env] table. ${NAME} and ${NAME:-default} in a value are filled in from haest's environment").short('E'),
    def("mods", WORK, Bool, "true", "Your mods in <config dir>/mods"),
    def("project_mods", WORK, Bool, "true", "The project's mods in .haest/mods (its code runs; a trusted dir only)"),
    def("approvals", WORK, Bool, "false", "The built-in approval mod: asks before a tool changes anything"),
    def("trusted_dirs", WORK, List(None), "~/projects,~/work", "Dirs whose project files haest reads (.haest/config.toml, .mcp.json, .haest/mods), with every dir inside them").scope(Scope::User),
    def("max_turn_usd", CAPS, Number, "5", "One turn's spend (0 = no cap)"),
    def("max_turn_tokens", CAPS, Number, "2000000", "One turn's tokens (unset: 10x the model's window; 0 = no cap)"),
    def("max_request_tokens", CAPS, Number, "150000", "One model call's input tokens (unset: 90% of the window; 0 = no cap)"),
    def("max_turn_steps", CAPS, Number, "200", "Model calls in one turn (0 = no cap)"),
    def("max_turn_minutes", CAPS, Number, "30", "One turn's wall clock (0 = no cap)"),
    def("max_repeats", CAPS, Number, "10", "The same tool call in a row before the guard stops it (0 = no cap)"),
    def("max_conversation_usd", CAPS, Number, "50", "The whole conversation's spend (0 = no cap)"),
    def("max_conversation_tokens", CAPS, Number, "50000000", "Tokens of models with no price (0 = no cap)"),
    def("context_window", CAPS, Number, "1000000", "The model's window in tokens (unset: the catalog's, else 200000)"),
    def("compact_at", CAPS, Number, "300000", "Compact the conversation past this many tokens (0 = at 80% of the window only)"),
    def("max_agent_turns", AGENTS, Number, "10", "Turns in a row a job or an agent starts with no human message; past it haest holds them (0 = no cap)"),
    def("max_agent_depth", AGENTS, Number, "1", "How deep agents nest, main = 0 (0 = no Agent tool)"),
    def("max_subagents", AGENTS, Number, "8", "Subagents running at once, across the tree (0 = no Agent tool)"),
    def("subagent_usd", AGENTS, Number, "0.5", "One subagent's spend"),
    def("subagent_steps", AGENTS, Number, "25", "One subagent's model calls"),
    def("subagent_minutes", AGENTS, Number, "5", "One subagent's wall clock"),
    def("subagent_budgets", AGENTS, Bool, "true", "The Agent tool takes a budget {usd, minutes}: tighter caps for one subagent, its spend held on the conversation's budget while it runs, its time fitted to what is left of the turn")
        .detail("Off, the Agent tool's parameters are Claude Code's. On, each value only tightens `subagent_usd` and `subagent_minutes` (an absent one is the cap); a spawn whose spend or time does not fit what is left is refused with what is left, so the model can ask for less. Subagents running at once hold their spend together, so they cannot overshoot `max_conversation_usd` together."),
    def("event_ring", SESSION, Number, "5000", "Frames kept for a peer that catches up on the conversation's stream, besides the running turn"),
    def("stdio", SESSION, Bool, "true", "Be driven by the parent process: requests in on stdin, frames out on stdout, no terminal")
        .scope(Scope::Process)
        .detail("One JSON object per line both ways, in haest-wire's format: `hello` first, then every frame of the conversation from the start, and a `reply` to each request (a turn's at its end). The parent may do everything a terminal may: turns (a person's input), `interrupt`, model and effort changes, `clear`, `close`, and the answers to approval asks. haest runs until stdin closes, once what it was asked has finished, or until a `close`. Never from a file: a config file cannot turn a terminal into a pipe."),
    def("control_socket", SESSION, Choice(ControlSocket::WORDS), "watch", "A Unix socket another program on this machine reaches the conversation through (haest ctl): off, watch (its stream, a snapshot, the history) or command (turns, interrupts, changes and approval answers too)")
        .scope(Scope::User)
        .detail("The socket goes in `control_dir`, named after the conversation's stream id and readable by you only (the dir 0700, the socket 0600), with a discovery file beside it that `haest ctl list` reads; both go when haest exits. What arrives over a command socket is another program's input: it counts against `max_agent_turns`, and at an open prompt it waits for Enter. Never from a project's file: a repository cannot open a socket into your conversation."),
    def("control_dir", SESSION, Path, "~/.haest/control", "Where control sockets and their discovery files go (unset: <data_dir>/control)").scope(Scope::User),
    def("broker_url", SESSION, Text, "wss://broker.example/agents", "A broker the conversation links to over a WebSocket (ws:// or wss://), so a fleet view elsewhere watches it; its token is the key HAEST_BROKER_TOKEN")
        .scope(Scope::User)
        .detail("haest dials out: the first frame is `hello` with the token in it (never in the URL), the broker answers with the last seq it holds, and the stream resumes there; a link that drops is dialed again, after a random wait that doubles each try (at most 30 s). A broker that refuses the token (close 4401 or 4408, or a 401 or 403 handshake) is not dialed again, and haest says so. Never from a project's file: a repository cannot send your conversation somewhere."),
    def("broker_access", SESSION, Choice(BrokerAccess::WORDS), "command", "What the broker may do: watch (the stream, a snapshot, the history) or command (turns, interrupts, changes and approval answers too)").scope(Scope::User),
    def("data_dir", PATHS, Path, "~/.haest/data", "Sessions, history and spill files (unset: $XDG_DATA_HOME/haest, else ~/.local/share/haest)"),
    def("config_dir", PATHS, Path, "~/dotfiles/haest", "config.toml, providers and mods (unset: $XDG_CONFIG_HOME/haest, else ~/.config/haest)").scope(Scope::Process),
    def("debug", PATHS, Bool, "true", "Log what haest does under the hood"),
];

pub fn find(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

/// A setting's flag, without the dashes in front: `skill_catalog` -> `skill-catalog`.
pub fn flag_name(key: &str) -> String {
    key.replace('_', "-")
}
