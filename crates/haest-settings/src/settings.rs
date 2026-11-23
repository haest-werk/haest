//! [`Settings`], the values a conversation is built from. A host writes them
//! out (`Settings { tools: BuiltinTools::ALL, ..Settings::bare(cwd, home) }`)
//! or has a loader fill them in (haest-config: files, `HAEST_*` variables,
//! flags).

use std::path::PathBuf;

use haest_core::{
    Effort, WorkspaceMode,
    cache::CacheTtl,
    caps::SubagentCaps,
    deferred_tools::DeferMode,
    provider::{WebSearchEngine, WebSearchOptions},
    skills::SkillSources,
    wind_down::WindDown,
};

use crate::{CapSettings, ContextSources, sets::BuiltinTools};

/// The model a run uses when none is configured: [`Settings::bare`]'s `model_spec`.
pub const DEFAULT_MODEL: &str = "openrouter:anthropic/claude-sonnet-5.5";

/// Everything a conversation is built from, as plain values.
///
/// Start from [`Settings::bare`] and change the fields you want; nothing here
/// is read from a file, a variable or a flag, and nothing touches a disk (the
/// paths are only values until a runtime uses them). Each field is the haest
/// setting of the same name, so `skill_catalog` is also the TOML key
/// haest-config reads into it, `HAEST_SKILL_CATALOG` and `--skill-catalog`
/// (`model_spec` is the `model` setting); the caps are grouped in
/// [`CapSettings`] and haest-core's [`SubagentCaps`].
///
/// The helper methods turn fields into what the loop and the tools take:
/// [`turn_caps_for`](Self::turn_caps_for), [`conversation_caps`](Self::conversation_caps),
/// [`web_search_options`](Self::web_search_options) and [`spill_dir`](Self::spill_dir).
///
/// # Examples
///
/// ```
/// use haest_settings::{BuiltinTools, ContextSources, Settings};
///
/// // A review run: reads the project's AGENTS.md, may read and search, never writes.
/// let review = Settings {
///     model_spec: "anthropic:claude-opus-5-5".into(),
///     context: ContextSources { agents: true, ..ContextSources::NONE },
///     tools: BuiltinTools { read: true, grep: true, glob: true, ..BuiltinTools::NONE },
///     append_system_prompt: Some("Review only; do not change files.".into()),
///     ..Settings::bare("/work/project", "/home/me")
/// };
/// assert_eq!(review.data_dir, std::path::PathBuf::from("/home/me/.local/share/haest"));
/// ```
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per on/off setting of the settings file, each independent"
)]
pub struct Settings {
    /// The working directory: relative paths resolve against it, and
    /// instruction files and skills are looked for from it upward.
    pub cwd: PathBuf,
    /// The human's home: where `~/.agents` and `~/.claude` are, and where
    /// [`Settings::bare`] puts `data_dir` and `config_dir`.
    pub home: PathBuf,
    /// The `model` setting: the model spec, `provider:model`
    /// (`openrouter:anthropic/claude-sonnet-5.5`).
    /// Bare: [`DEFAULT_MODEL`].
    pub model_spec: String,
    /// Reasoning effort; None sends none, and the model uses its own default.
    pub effort: Option<Effort>,
    /// How long the provider keeps the prompt cache each model call writes:
    /// five minutes, or an hour, which costs more to write and pays when the
    /// human pauses for longer than five minutes (haest-core's
    /// [`cache`](haest_core::cache) says when). Anthropic's API and
    /// OpenRouter's `anthropic/` models take it; other providers ignore it.
    /// Bare: [`CacheTtl::FiveMinutes`].
    pub cache_ttl: CacheTtl,
    /// Replaces haest's own identity text at the top of the system prompt; None keeps it.
    pub system_prompt: Option<String>,
    /// Text added at the end of the system prompt.
    pub append_system_prompt: Option<String>,
    /// Which instruction files (AGENTS.md, CLAUDE.md) go in the system prompt.
    pub context: ContextSources,
    /// Where skills are found (`~/.agents/skills`, `.claude/skills`, plugins).
    pub skills: SkillSources,
    /// List the skills in the system prompt, so the model picks them on its own.
    pub skill_catalog: bool,
    /// The built-in tools the model gets.
    pub tools: BuiltinTools,
    /// The Code tool: a JavaScript run that calls the other tools.
    pub code: bool,
    /// Extra tool names Code may call (exact, or `prefix*`); read only when `code` is on.
    pub code_tools: Vec<String>,
    /// Agent, SendMessage and StopAgent: the model may start subagents.
    pub agents: bool,
    /// The provider's web search, by engine; None = off. Only OpenRouter runs one.
    pub web_search: Option<WebSearchEngine>,
    /// GenerateImage, over OpenRouter with the host's `OPENROUTER_API_KEY`.
    pub image: bool,
    /// GenerateImage's default model, an OpenRouter model id; None = the
    /// tool's own (haest-tools' `DEFAULT_IMAGE_MODEL`).
    pub image_model: Option<String>,
    /// The other models a GenerateImage call may pick; None = the tool's
    /// list (haest-tools' `DEFAULT_IMAGE_MODELS`).
    pub image_models: Option<Vec<String>>,
    /// How MCP tools reach the model: behind ToolSearch, or all on every call.
    pub deferred_tools: DeferMode,
    /// Where the tools work: the sandbox (a simulated bash over a copy-on-write
    /// overlay, nothing reaches the disk until applied), the real disk and
    /// bash, or no files and no shell at all.
    pub workspace: WorkspaceMode,
    /// The human's mods in `<config_dir>/mods`.
    pub mods: bool,
    /// The project's mods in `.haest/mods`: the repository's own code runs.
    ///
    /// No trust check happens here: haest-config turns this on only inside
    /// `trusted_dirs`. A host that sets it runs whatever `.haest/mods` holds.
    pub project_mods: bool,
    /// The built-in approval mod: in the real workspace, asks before a shell
    /// command that cannot be undone. Bare: on.
    pub approvals: bool,
    /// The spend, token, step and time caps.
    pub caps: CapSettings,
    /// The share of a cap (a turn's, a subagent's, the conversation's) at
    /// which a run stops starting subagents and the model is told once to
    /// conclude: haest-core's [`WindDown`]. Bare: off.
    pub wind_down_at: WindDown,
    /// Turns in a row a background job or an agent may start with no human
    /// message; past it haest holds them. 0 = no cap.
    pub max_agent_turns: u32,
    /// The caps on the subagent tree, for haest-tools' hub: depth, how many
    /// at once, and each one's budget (the `max_agent_depth`,
    /// `max_subagents`, `subagent_usd`, `subagent_steps`, `subagent_minutes`
    /// and `subagent_budgets` settings).
    pub subagents: SubagentCaps,
    /// Where sessions, the REPL's history and spilled tool output go.
    pub data_dir: PathBuf,
    /// haest's own config dir; the human's mods are in `<config_dir>/mods`.
    pub config_dir: PathBuf,
    /// Log what haest does under the hood.
    pub debug: bool,
}

impl Settings {
    /// Bare haest for a project at `cwd`: every capability off until asked
    /// for, every brake on.
    ///
    /// No tools, no instruction files, no skills, no Code, no agents, no web
    /// search, no image tool, no mods, no wind-down; approvals on, the sandbox workspace,
    /// [`DEFAULT_MODEL`], MCP tools behind ToolSearch, and the caps in
    /// [`CapSettings`] at their bare values (2 USD and 100 model calls a turn, 20
    /// USD a conversation, ...). `data_dir` is `<home>/.local/share/haest` and
    /// `config_dir` `<home>/.config/haest`. Neither path is checked or created.
    ///
    /// ```
    /// use haest_settings::Settings;
    /// use haest_core::WorkspaceMode;
    ///
    /// let s = Settings::bare("/work", "/home/me");
    /// assert!(s.tools.is_empty() && s.context.is_empty() && !s.agents);
    /// assert!(s.approvals && s.workspace == WorkspaceMode::Sandbox);
    /// ```
    pub fn bare(cwd: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            cwd: cwd.into(),
            model_spec: DEFAULT_MODEL.into(),
            effort: None,
            cache_ttl: CacheTtl::FiveMinutes,
            system_prompt: None,
            append_system_prompt: None,
            context: ContextSources::NONE,
            skills: SkillSources::default(),
            skill_catalog: false,
            tools: BuiltinTools::NONE,
            code: false,
            code_tools: Vec::new(),
            agents: false,
            web_search: None,
            image: false,
            image_model: None,
            image_models: None,
            deferred_tools: DeferMode::Proxy,
            workspace: WorkspaceMode::Sandbox,
            mods: false,
            project_mods: false,
            approvals: true,
            caps: CapSettings::default(),
            wind_down_at: WindDown::OFF,
            max_agent_turns: 30,
            subagents: SubagentCaps::default(),
            data_dir: home.join(".local/share/haest"),
            config_dir: home.join(".config/haest"),
            debug: false,
            home,
        }
    }

    /// The web search a model call carries, or None when `web_search` is off.
    /// The result caps are haest's ([`WebSearchOptions::new`]: 5 a search, 15 a
    /// model call).
    pub fn web_search_options(&self) -> Option<WebSearchOptions> {
        self.web_search.map(WebSearchOptions::new)
    }

    /// Where one session's spilled tool output goes:
    /// `<data_dir>/spill/<session_id>`. A tool result too big for the context
    /// is written there and the model gets its path instead.
    pub fn spill_dir(&self, session_id: &str) -> PathBuf {
        self.data_dir.join("spill").join(session_id)
    }
}
