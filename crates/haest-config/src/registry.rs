//! Every setting haest has, once, with the [`Config`] field it is. A setting
//! has one name written three ways: `skill_catalog` in a TOML file,
//! `HAEST_SKILL_CATALOG` in the environment, `--skill-catalog` on the command
//! line. Its default is the bare value (`Config::bare`): every capability off
//! until asked for, every brake on.

use haest_core::{Effort, deferred_tools::DeferMode, provider::WebSearchEngine};
use haest_mods::HookSettings;
use haest_settings::{BuiltinTools, ContextSources, ModReload, SkillSources};

use crate::fields::{
    BrokerAccess, Config, ControlSocket, ENV, EnvPassthrough, Get, Raw, Set, WEB_SEARCH,
};

/// `off`, then every engine.
const WEB_SEARCH_WORDS: [&str; WebSearchEngine::WORDS.len() + 1] = {
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
/// More kinds may come in a minor release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// true / false (also 1 / 0, on / off, yes / no).
    Bool,
    /// A number >= 0. A setting that counts something (steps, tokens) takes a
    /// whole number only: a fraction is refused, never cut to a smaller count.
    Number,
    /// Any text, as written.
    Text,
    /// A URL haest dials, as written. One with an `@` past its host is
    /// refused: a URL parser ends the host at the first `/`, `?` or `#`, so
    /// a password holding one unescaped would send the rest of it, and the
    /// host after the `@`, to another host. So is one with no host
    /// (`wss:///agents`, `wss://:8443/agents`: what an empty `${HOST}`
    /// makes). Both are haest-http's `check_endpoint_url`.
    Url,
    /// A dir: `~/x` is in the home dir, `x` in the working dir.
    Path,
    /// One of these words.
    Choice(&'static [&'static str]),
    /// A comma list, or a TOML array. `Some` = only these words, plus `all`
    /// (every one of them) and `none` (an empty list).
    List(Option<&'static [&'static str]>),
    /// A list of name patterns, written as a `List(None)` is: each an exact
    /// name or a prefix ending in `*`. One that is neither (a regular
    /// expression such as `Edit|Write`, a `*` inside) would match nothing,
    /// and is refused.
    Patterns,
    /// Variables, one `NAME=VALUE` each: a repeated flag, a TOML table. A
    /// value adds to the ones before it; a name set twice keeps the later value.
    Vars,
    /// A table of settings: `[key]` in a file, holding values, tables
    /// (`[key.sub."name"]`) and arrays of tables (`[[key.list]]`). A
    /// variable or a flag holds the same as TOML: `key = value` lines, or an
    /// inline table `{ key = value }`; the flag may be repeated. Each value
    /// merges into the ones before it: a table key by key, an array appended
    /// to, any other value replaced. Its field's type says which keys and
    /// values it takes; a key it lacks is refused.
    Table,
}

/// Where a setting may come from. A layer that sets one it may not is an
/// error naming the file. haest may add a scope in a minor release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
/// # fn main() -> Result<(), &'static str> {
/// let catalog = DEFS.iter().find(|d| d.key == "skill_catalog").ok_or("no skill_catalog")?;
/// assert_eq!(catalog.env(), "HAEST_SKILL_CATALOG");
/// assert_eq!(catalog.flag(), "skill-catalog");
/// assert_eq!(catalog.kind, Kind::Bool);
///
/// let model = DEFS.iter().find(|d| d.key == "model").ok_or("no model")?;
/// assert_eq!(model.short, Some('m'));
/// # Ok(())
/// # }
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

use Kind::{Bool, Choice, List, Number, Path, Patterns, Table, Text, Url, Vars};

/// A row's field, put to one use: `get_set`, how [`Config`] reads and writes
/// it by its [`Raw`]; or, with a function's name, what that function says of
/// the field's type. `own(PAIR)` is a setting with a get and a set of its
/// own, and no type.
macro_rules! field {
    (get_set, own($pair:ident)) => {
        $pair
    };
    ($of:ident, own($pair:ident)) => {
        None
    };
    (get_set, $($path:ident).+) => {
        (
            |c| Raw::to_raw(&c.$($path).+),
            |c, raw| {
                c.$($path).+ = Raw::over(&c.$($path).+, raw)?;
                Ok(())
            },
        )
    };
    ($of:ident, $($path:ident).+) => {
        $of(|c| &c.$($path).+)
    };
}

/// Every setting, one row each: its key, the [`Config`] field it is (or
/// `own(PAIR)`), and its [`Def`] but the key. Makes [`DEFS`], the fields
/// [`Config::get`] and [`Config::set`] go by, and the bounds a number is
/// checked against.
macro_rules! settings {
    ($($key:literal => $($field:ident).+ $(($own:ident))?: def($($arg:expr),+) $(.$with:ident($value:expr))*,)+) => {
        /// Every setting haest has, one row each, in the order `--help` and
        /// `haest settings` list them: key, heading, kind, an example value, help.
        ///
        /// The one list: the loader checks every value against it, a host builds its
        /// flags from it, and
        /// [docs/settings.md](https://github.com/haest-werk/haest/blob/main/docs/settings.md)
        /// is written from it.
        pub const DEFS: &[Def] = &[$(def($key, $($arg),+)$(.$with($value))*,)+];

        /// Each setting's get and set, by its key.
        pub(crate) const FIELDS: &[(&str, (Get, Set))] =
            &[$(($key, field!(get_set, $($field).+ $(($own))?)),)+];

        /// Whether `key`'s field is a count, and its largest value: what its
        /// type says ([`Raw::WHOLE_UP_TO`]), so the check and the field agree.
        pub(crate) fn whole_up_to(key: &str) -> Option<f64> {
            fn of<T: Raw>(_: fn(&Config) -> &T) -> Option<f64> {
                T::WHOLE_UP_TO
            }
            match key {
                $($key => field!(of, $($field).+ $(($own))?),)+
                _ => None,
            }
        }

        /// The largest number `key`'s field takes, when its type has one
        /// ([`Raw::UP_TO`]): a share is at most 1.
        pub(crate) fn up_to(key: &str) -> Option<f64> {
            fn of<T: Raw>(_: fn(&Config) -> &T) -> Option<f64> {
                T::UP_TO
            }
            match key {
                $($key => field!(of, $($field).+ $(($own))?),)+
                _ => None,
            }
        }
    };
}

settings! {
    "profile" => profile: def(PROFILE, Text, "claude", "A named set of settings: claude (built in: everything Claude Code has that haest reads), or a [profiles.<name>] table in a config file"),
    "model" => settings.model_spec: def(MODEL, Text, "anthropic:claude-opus-5-5", "provider:model").short('m'),
    "effort" => settings.effort: def(MODEL, Choice(Effort::WORDS), "high", "Reasoning effort (unset: the model's own default)").short('e'),
    "cache" => settings.cache: def(MODEL, Table, r#"{ system = "1h", keep_warm = true }"#, "The prompt cache: which blocks carry a breakpoint and for how long (system, tools, history: off, 5m or 1h), and what an idle conversation does before its cache expires (keep_warm, keep_warm_minutes, compact_before_expire); per model under models.\"<spec or prefix*>\"")
        .detail("`system`, `tools` and `history` (the newest message, which the next request reads the conversation before it from) each take `off`, `5m` or `1h`; the default is `5m` for all three. `1h` costs more to write (2x the input price against 1.25x on Anthropic) and pays when you often pause for longer than five minutes; a model that keeps no cache entry for an hour gets five minutes, and you are told. `keep_warm = true` sends a pre-warm a little before an idle conversation's cache expires, while the model's prices say the pings cost less than writing the cache again, for at most `keep_warm_minutes` after the last turn (default 60); a ping that wrote instead of read stops it, and you are told. A model with no price, or a provider that takes no pre-warm (only Anthropic's API and OpenRouter's `anthropic/` models do), is never pinged. `compact_before_expire = { min_context = 120000, instructions = \"...\" }` (`min_context` is a whole number of tokens, or a share of the window between 0 and 1: `0.6`; default `\"off\"`) compacts an idle conversation that holds at least `min_context` while its cache is still warm, when the summary and the smaller next write cost less than writing the whole prompt again; when they would not, `keep_warm` still pings. Only a conversation that stays open between turns idles: the interactive prompt, or a session another program drives. `[cache.models.\"openrouter:google/*\"]` sets any of these for the models it matches (exact, or `prefix*`); the most specific wins. A key that is neither (a regular expression, a `*` inside) would match no model, and is refused."),
    "system_prompt" => settings.system_prompt: def(MODEL, Text, "You are a terse code reviewer.", "Replaces haest's own identity text at the top of the system prompt"),
    "append_system_prompt" => settings.append_system_prompt: def(MODEL, Text, "Answer in British English.", "Text appended to the system prompt"),
    "provider_routing" => provider_routing: def(MODEL, Text, r#"{"global": {"banned": ["novita"]}}"#, "OpenRouter's provider routing, as JSON: {\"global\": {\"banned\": [...], \"favorites\": [...]}, \"overrides\": ..., \"project\": ...}").scope(Scope::User),
    "context" => settings.context: def(CONTEXT, List(Some(ContextSources::WORDS)), "agents", "Instruction files: agents (AGENTS.md: ~/.agents/AGENTS.md, then every dir down to here), claude (CLAUDE.md, .claude/CLAUDE.md, CLAUDE.local.md, ~/.claude/CLAUDE.md)"),
    "skills" => settings.skills: def(CONTEXT, List(Some(SkillSources::WORDS)), "agents,claude", "Where skills come from: agents (~/.agents/skills, .agents/skills), claude (~/.claude/skills, .claude/skills), plugins (Claude Code's installed plugins); /<name> runs one"),
    "skill_catalog" => settings.skill_catalog: def(CONTEXT, Bool, "true", "List the skills in the system prompt, so the model picks them on its own"),
    "tools" => settings.tools: def(TOOLS, List(Some(BuiltinTools::WORDS)), "read,grep,glob,bash", "Built-in tools: read, write, edit, bash, grep, glob, web_fetch, tasks, skill (all = every one)"),
    "code" => settings.code: def(TOOLS, Bool, "true", "The Code tool: a JavaScript run that calls the other tools"),
    "code_tools" => settings.code_tools: def(TOOLS, Patterns, "Bash,mcp__github__*", "Extra tool names Code may call (exact, or prefix*)"),
    "agents" => settings.agents: def(TOOLS, Bool, "true", "Agent, SendMessage and StopAgent: subagents"),
    "web_search" => own(WEB_SEARCH): def(TOOLS, Choice(&WEB_SEARCH_WORDS), "native", "The provider's web search, by engine"),
    "image" => settings.image: def(TOOLS, Bool, "true", "GenerateImage, over OpenRouter (needs OPENROUTER_API_KEY)"),
    "image_model" => settings.image_model: def(TOOLS, Text, "black-forest-labs/flux-3-image", "GenerateImage's default model"),
    "image_models" => settings.image_models: def(TOOLS, List(None), "openai/gpt-image-2.6-sunburst,black-forest-labs/flux-3-image", "The other image models a call may pick"),
    "mcp" => mcp: def(TOOLS, Bool, "true", "Start the project's MCP servers (.mcp.json); a trusted dir only"),
    "deferred_tools" => settings.deferred_tools: def(TOOLS, Choice(DeferMode::WORDS), "grow", "How MCP tools reach the model: behind ToolSearch (proxy, grow) or all up front (off)")
        .detail("ToolSearch lists each held-back tool with its description's first sentence. A host linking haest can hold back its own tools too, all but a list it names (`ToolDeferral::deferring_all_except`, `DeferredTools`); a held-back tool the model calls by name still runs, and loads."),
    "workspace" => settings.workspace: def(WORK, Choice(WORKSPACES), "real", "sandbox: a simulated bash over a copy-on-write overlay (nothing reaches the disk until /apply); real: the real disk and bash").short('w'),
    "env_passthrough" => env_passthrough: def(WORK, Choice(EnvPassthrough::WORDS), "none", "What a process haest starts (bash, a background job, an MCP server) gets of haest's environment: none, core (PATH, HOME, the user, the locale, the temp dirs), all (every variable, API keys included)"),
    "env" => own(ENV): def(WORK, Vars, "GH_TOKEN=${GITHUB_TOKEN}", "A variable every process haest starts gets, the sandbox's shell included, over env_passthrough: NAME=VALUE, or NAME for haest's own value; repeat it for more. In a file: an [env] table. ${NAME} and ${NAME:-default} in a value are filled in from haest's environment").short('E'),
    "mods" => settings.mods: def(WORK, Bool, "true", "Your mods in <config dir>/mods"),
    "project_mods" => settings.project_mods: def(WORK, Bool, "true", "The project's mods in .haest/mods (its code runs; a trusted dir only)"),
    "mod_reload" => settings.mod_reload: def(WORK, Choice(ModReload::WORDS), "ask", "Whether mod files reload as they change, checked before each model call: off, ask (a new, changed or deleted file waits for your Allow) or auto (it loads or unloads without asking)")
        .detail("A deleted file unloads, and its tools leave the model's list. A file that fails to load keeps the version that ran, and haest says why. After a Deny the old version stays, and the same content is not asked about again. Rust mods never reload.")
        .scope(Scope::User),
    "hooks" => hooks: def(WORK, Table, r#"{ "tool.before" = [{ command = "sh ~/.config/haest/hooks/no-rm-rf.sh", dialect = "claude", matcher = "Bash" }] }"#, "Commands and URLs run at each event of the mods protocol, with no Rust: [[hooks.\"<event>\"]] entries, each a command (run on the host) or a url (POSTed), in haest's JSON or Claude Code's (dialect = \"claude\")")
        .detail("The events are the protocol's own names, quoted for their dot: `tool.before`, `tool.after`, `prompt.submit`, `system.prompt`, `model.before`, `model.after`, `turn.end`. An entry takes `command` or `url` (one of them), `dialect` (`haest` or `claude`; `claude` runs a Claude Code hook script as it is, on the events Claude Code has, so never on `model.before` or `model.after`), `matcher` (on `tool.before` and `tool.after` only: a tool's name, or a prefix ending in `*`), `timeout` (seconds, Claude Code's key as it is: every other time setting that takes a number takes minutes, `[timeouts]` takes text with its unit, and the cache's TTLs are the words `5m` and `1h`; default `[timeouts] hook`, a minute), `on_error` (`block` or `continue`; default `block` on `tool.before`, else `continue`) and `headers` (a `url`'s, `{ name = \"value\" }`, `${NAME}` filled in from haest's environment). A command runs on the host (never in the sandbox) in the working dir, with the environment every process haest starts gets (`env_passthrough`, `env`): give one in your own file as an absolute path or under `~/`, since a relative one runs whatever the working dir has there. Every layer's entries bind after the ones before it, in the order written, as one mod after the built-in ones and your mod files. A subagent's tool calls pass through them and its system prompt takes what `system.prompt` adds; `prompt.submit`, `model.before`, `model.after` and `turn.end` are the main conversation's. `haest settings` shows a header by its name only. [hooks.md](hooks.md) has the whole story: every event's input and every answer it takes, in both dialects, and every key."),
    "approvals" => settings.approvals: def(WORK, Bool, "false", "The built-in approval mod: in the real workspace, asks before a shell command that cannot be undone (deleting recursively, rewriting git history or the remote, wiping a disk, killing processes by name, sudo); anything else runs without asking")
        .detail("haest asks on a terminal only where the question can be seen and answered: stdin must be a terminal, and so must stderr, where the question shows (with --json it is a line on stdout, which must not be a regular file). Otherwise each approval is refused at once, and haest says why on stderr."),
    "trusted_dirs" => trusted_dirs: def(WORK, List(None), "~/projects,~/work", "Dirs whose project files haest reads (.haest/config.toml, .mcp.json, .haest/mods), with every dir inside them").scope(Scope::User),
    "max_turn_usd" => settings.caps.max_turn_usd: def(CAPS, Number, "5", "One turn's spend (0 = no cap)"),
    "max_turn_tokens" => settings.caps.max_turn_tokens: def(CAPS, Number, "2000000", "One turn's tokens (unset: 10x the model's window; 0 = no cap)"),
    "max_request_tokens" => settings.caps.max_request_tokens: def(CAPS, Number, "150000", "One model call's input tokens (unset: 90% of the window; 0 = no cap)"),
    "max_turn_steps" => settings.caps.max_turn_steps: def(CAPS, Number, "200", "Model calls in one turn (0 = no cap)"),
    "max_turn_minutes" => settings.caps.max_turn_time: def(CAPS, Number, "30", "One turn's wall clock (0 = no cap)"),
    "max_repeats" => settings.caps.max_repeats: def(CAPS, Number, "10", "The same tool call in a row before the guard stops it (0 = no cap)"),
    "max_conversation_usd" => settings.caps.max_conversation_usd: def(CAPS, Number, "50", "The whole conversation's spend (0 = no cap)"),
    "max_conversation_tokens" => settings.caps.max_conversation_tokens: def(CAPS, Number, "50000000", "Tokens of models with no price (0 = no cap)"),
    "wind_down_at" => settings.wind_down_at: def(CAPS, Number, "0.8", "At this share of a cap, no new subagents and the model is told once to conclude (0 = off)")
        .detail("Watches the conversation's USD and unpriced tokens, and the turn's USD, tokens and wall clock. A subagent watches the conversation's and its own (subagent_usd, subagent_minutes, max_turn_tokens) the same way, and winds down alone. One share for every cap. The guard's nudge at half a cap is separate and stays on."),
    "context_window" => settings.caps.context_window: def(CAPS, Number, "1000000", "The model's window in tokens (unset: the catalog's, else 200000)"),
    "compact_at" => settings.caps.compact_at: def(CAPS, Number, "300000", "Compact the conversation past this many tokens (0 = at 80% of the window only)"),
    "timeouts" => settings.timeouts: def(CAPS, Table, r#"{ hook = "30s", javascript = { code = "20m" } }"#, "How long haest waits before it gives up: on a hook (hook), on a human's Allow/Deny (ask), on a shell command (bash, bash_max), on a model's answer to start and to go on (model_headers, model_stall), on an MCP server (mcp_connect, mcp_call), on a web page (web_fetch), and on its JavaScript (javascript: code, sandbox, mod_load, mod_call, mod_slice); each is text with its unit, like \"30s\" or \"10m\"")
        .detail("A value is a number and its unit, `ms`, `s`, `m` or `h` (`\"500ms\"`, `\"30s\"`, `\"1.5h\"`), above 0. A number alone is refused: `30` could be seconds or minutes. `hook` (default `\"60s\"`): a `[[hooks.<event>]]` entry that sets no `timeout` of its own; past it, the hook failed and its `on_error` decides. `ask` (`\"10m\"`): an Allow/Deny prompt, the approval mod's or a mod's; unanswered, it is a Deny. `bash` (`\"2m\"`): a Bash call that names no timeout of its own. `bash_max` (`\"10m\"`): the most a Bash call may ask for, and the most one sandbox shell call runs; a `bash` above it raises it to `bash`. A background job has no timeout. `model_headers` (`\"10m\"`): one try of a model call, waiting for the provider's status and headers; past it the try is retried like a dropped connection. A stream that has started is not timed by it. A host that hands in its own models keeps their own. `model_stall` (unset: off): how long a model's answer, once its stream has started, may send nothing; past it the answer ends as an error, with the text so far, and is not retried. Unset, a silent stream waits for the turn's cap (`max_turn_minutes`) or a cancel. `mcp_connect` (`\"15s\"`): an MCP server's start and `initialize`, and again its tool list; past it the server did not start. A server run with `npx` or `uvx` may need longer the first time, while its package downloads. `mcp_call` (`\"10m\"`): one call to an MCP server's tool; past it the call fails and is cancelled on the server. `web_fetch` (`\"30s\"`): one WebFetch call, every redirect and the body included. `javascript.code` (`\"10m\"`): a Code tool run, wall clock, the tools it calls included; the model may ask for less on a call, never for more. `javascript.sandbox` (`\"5m\"`): the sandbox `js` command, never past the shell call it runs in. `javascript.mod_load` (`\"30s\"`): a mod file's default export, as the mod loads. `javascript.mod_call` (`\"60s\"`): one call into a loaded mod file (a handler, a tool, a command), awaiting included, the time a `haest.ask` waits on the human not; past it that call fails and the mod runs on. `javascript.mod_slice` (`\"10s\"`): a mod file's code without a pause, what stops a CPU loop in a mod. The turn's caps (`max_turn_minutes`, `subagent_minutes`) hold over all of it."),
    "limits" => settings.limits: def(CAPS, Table, r#"{ javascript = { code_memory = "256MiB" } }"#, "How much memory each kind of JavaScript run gets (javascript: code_memory, sandbox_memory, mod_memory); each is text with its unit, like \"64MiB\" or \"1GiB\"")
        .detail("A value is a number and its unit, `B`, `KB`, `MB`, `GB` (powers of 1000) or `KiB`, `MiB`, `GiB` (powers of 1024), in any case, 1MiB at least. A number alone is refused. Each is one engine's heap: an allocation past it is an out-of-memory error inside the script. `javascript.code_memory` (default `\"64MiB\"`): a Code tool run. `javascript.sandbox_memory` (`\"64MiB\"`): the sandbox `js` command. `javascript.mod_memory` (`\"128MiB\"`): each mod file, for as long as it is loaded."),
    "max_agent_turns" => settings.max_agent_turns: def(AGENTS, Number, "10", "Turns in a row a job or an agent starts with no human message; past it haest holds them (0 = no cap)"),
    "max_agent_depth" => settings.subagents.max_depth: def(AGENTS, Number, "1", "How deep agents nest, main = 0 (0 = no Agent tool)"),
    "max_subagents" => settings.subagents.max_concurrent: def(AGENTS, Number, "8", "Subagents running at once, across the tree (0 = no Agent tool)"),
    "subagent_usd" => settings.subagents.usd: def(AGENTS, Number, "0.5", "One subagent's spend"),
    "subagent_steps" => settings.subagents.steps: def(AGENTS, Number, "25", "One subagent's model calls"),
    "subagent_minutes" => settings.subagents.time: def(AGENTS, Number, "5", "One subagent's wall clock"),
    "subagent_budgets" => settings.subagents.budgets: def(AGENTS, Bool, "true", "The Agent tool takes a budget {usd, minutes}: tighter caps for one subagent, its spend held on the conversation's budget while it runs, its time fitted to what is left of the turn")
        .detail("Off, the Agent tool's parameters are Claude Code's. On, each value only tightens `subagent_usd` and `subagent_minutes` (an absent one is the cap); a spawn whose spend or time does not fit what is left is refused with what is left, so the model can ask for less. Subagents running at once hold their spend together, so they cannot overshoot `max_conversation_usd` together."),
    "event_ring" => event_ring: def(SESSION, Number, "5000", "Frames kept for a peer that catches up on the conversation's stream, besides the running turn"),
    "stdio" => stdio: def(SESSION, Bool, "true", "Be driven by the parent process: requests in on stdin, frames out on stdout, no terminal")
        .scope(Scope::Process)
        .detail("One JSON object per line both ways, in haest-wire's format: `hello` first, then every frame of the conversation from the start, and a `reply` to each request (a turn's at its end). The parent may do everything a terminal may: turns (a person's input), `interrupt`, model and effort changes, `clear`, `close`, and the answers to approval asks. haest runs until stdin closes, once what it was asked has finished, or until a `close`. Never from a file: a config file cannot turn a terminal into a pipe."),
    "control_socket" => control_socket: def(SESSION, Choice(ControlSocket::WORDS), "watch", "A Unix socket another program on this machine reaches the conversation through (haest ctl): off, watch (its stream, a snapshot, the history) or command (turns, interrupts, changes and approval answers too)")
        .scope(Scope::User)
        .detail("The socket goes in `control_dir`, named after the conversation's stream id and readable by you only (the dir 0700, the socket 0600), with a discovery file beside it that `haest ctl list` reads; both go when haest exits. What arrives over a command socket is another program's input: it counts against `max_agent_turns`, and at an open prompt it waits for Enter. At a terminal, a command socket's programs are asked each approval too: the first answer counts, theirs or the one typed at the terminal, and the other side stops asking. Never from a project's file: a repository cannot open a socket into your conversation."),
    "control_dir" => control_dir: def(SESSION, Path, "~/.haest/control", "Where control sockets and their discovery files go (unset: <data_dir>/control)").scope(Scope::User),
    "broker_url" => broker_url: def(SESSION, Url, "wss://broker.example/agents", "A broker the conversation links to over a WebSocket (ws:// or wss://), so a fleet view elsewhere watches it; its token is the key HAEST_BROKER_TOKEN")
        .scope(Scope::User)
        .detail("haest dials out: the first frame is `hello` with the token in it (never in the URL), the broker answers with the last seq it holds, and the stream resumes there; a link that drops is dialed again, after a random wait that doubles each try (at most 30 s), and so is one the broker goes silent on (haest pings a broker it has not heard from in 20 s, and gives up on it after 60 s of silence). A broker that refuses the token (close 4401 or 4408, or a 401 or 403 handshake) is not dialed again, and haest says so. Never from a project's file: a repository cannot send your conversation somewhere."),
    "broker_access" => broker_access: def(SESSION, Choice(BrokerAccess::WORDS), "command", "What the broker may do: watch (the stream, a snapshot, the history) or command (turns, interrupts, changes and approval answers too)").scope(Scope::User),
    "data_dir" => settings.data_dir: def(PATHS, Path, "~/.haest/data", "Sessions, history and spill files (unset: $XDG_DATA_HOME/haest, else ~/.local/share/haest)"),
    "config_dir" => settings.config_dir: def(PATHS, Path, "~/dotfiles/haest", "config.toml, providers and mods (unset: $XDG_CONFIG_HOME/haest, else ~/.config/haest)").scope(Scope::Process),
    "debug" => settings.debug: def(PATHS, Bool, "true", "Log what haest does under the hood"),
}

pub(crate) fn find(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

/// What a trusted project's file may not put in a setting it may set: the
/// settings reference's words for it, and the check the project's value
/// (in the checked spelling) goes through, a profile the file defines
/// included.
pub(crate) struct ProjectLimit {
    /// What "Set in" adds.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "docs/settings.md is written by a test")
    )]
    pub(crate) says: &'static str,
    /// Why the project's value may not be, if it may not.
    pub(crate) check: fn(&str) -> Result<(), String>,
}

/// The limit on what a project's file puts in `key`, if it has one. A
/// `hooks` entry with a `url` POSTs the conversation's events off the
/// machine, where the repo says. A trusted project already runs its own
/// code (its mods, its `.mcp.json` servers), so it may bind a command;
/// never a URL.
pub(crate) fn project_limit(key: &str) -> Option<ProjectLimit> {
    match key {
        "hooks" => Some(ProjectLimit {
            says: "a `url` entry only in your own file, its variable or its flag, never in a project's file or a profile one defines",
            check: |raw| {
                let hooks: HookSettings = Raw::from_raw(raw)?;
                match hooks.url_entries().first() {
                    Some(entry) => Err(format!(
                        "{entry}: a `url` hook can't be set in a project's file: it would send your conversation's events where the repo says. Bind it in your own config file"
                    )),
                    None => Ok(()),
                }
            },
        }),
        _ => None,
    }
}

/// A setting's flag, without the dashes in front: `skill_catalog` -> `skill-catalog`.
pub(crate) fn flag_name(key: &str) -> String {
    key.replace('_', "-")
}
