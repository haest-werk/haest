//! [`Settings`], the values a conversation is built from, and [`Limits`], its
//! caps. A host writes them out
//! (`Settings { tools: BuiltinTools::ALL, ..Settings::bare(cwd, home) }`) or
//! has a loader fill them in (haest-config: files, `HAEST_*` variables, flags).

use std::path::PathBuf;

use haest_core::{
    Effort, WorkspaceMode,
    deferred_tools::DeferMode,
    ledger::LedgerCaps,
    limits::{SubagentCaps, TurnLimits},
    media::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS},
    provider::{WebSearchConfig, WebSearchEngine},
    skills::SkillSources,
};

use crate::{ContextSources, sets::BuiltinTools};

/// The model a run uses when none is configured: [`Settings::bare`]'s `model`.
pub const DEFAULT_MODEL: &str = "openrouter:anthropic/claude-sonnet-5.5";

/// The caps on one turn (one human message) and on the whole conversation.
///
/// 0 means no cap, for every field. A token cap left `None` follows the
/// model's context window instead; [`Settings::limits_for`] turns these into
/// the loop's per-turn [`TurnLimits`] and [`Settings::ledger`] into the
/// conversation's [`LedgerCaps`]. The loop checks them before each model call;
/// the human goes past a turn cap by sending another message.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// One turn's spend in USD; only calls whose cost is known count. Bare: 2.
    pub max_turn_usd: f64,
    /// One turn's billed tokens; None = 10x the model's window.
    pub max_turn_tokens: Option<u64>,
    /// One model call's estimated input tokens; None = 90% of the window.
    pub max_request_tokens: Option<u64>,
    /// Model calls in one turn. Bare: 100.
    pub max_turn_steps: u32,
    /// One turn's wall clock, in minutes. Bare: 60.
    pub max_turn_minutes: f64,
    /// The same tool call in a row before the guard stops it. Bare: 6.
    pub max_repeats: u32,
    /// The whole conversation's spend in USD. Bare: 20.
    pub max_conversation_usd: f64,
    /// Tokens billed by models with no price, across the conversation. Bare: 20M.
    pub max_conversation_tokens: u64,
    /// The model's context window in tokens; None (or 1000 or less) = the
    /// catalog's, else 200K. See [`Settings::window`].
    pub context_window: Option<u64>,
    /// Compact the conversation past this many tokens; 0 = at 80% of the window only. Bare: 400K.
    pub compact_at: u64,
}

/// Everything a conversation is built from, as plain values.
///
/// Start from [`Settings::bare`] and change the fields you want; nothing here
/// is read from a file, a variable or a flag, and nothing touches a disk (the
/// paths are only values until a runtime uses them). Each field is the haest
/// setting of the same name, so `skill_catalog` is also the TOML key
/// haest-config reads into it, `HAEST_SKILL_CATALOG` and `--skill-catalog`; the
/// caps are grouped in [`Limits`] and [`SubagentCaps`].
///
/// The helper methods turn fields into what the loop and the tools take:
/// [`limits_for`](Self::limits_for), [`ledger`](Self::ledger),
/// [`code_tools`](Self::code_tools()), [`web_search_config`](Self::web_search_config),
/// [`image_models`](Self::image_models()) and [`spill_dir`](Self::spill_dir).
///
/// # Examples
///
/// ```
/// use haest_settings::{BuiltinTools, ContextSources, Settings};
///
/// // A review run: reads the project's AGENTS.md, may read and search, never writes.
/// let review = Settings {
///     model: "anthropic:claude-opus-5-5".into(),
///     context: ContextSources { agents: true, ..ContextSources::NONE },
///     tools: BuiltinTools { read: true, grep: true, glob: true, ..BuiltinTools::NONE },
///     append_system_prompt: Some("Review only; do not change files.".into()),
///     ..Settings::bare("/work/project", "/home/me")
/// };
/// assert_eq!(review.data_dir, std::path::PathBuf::from("/home/me/.local/share/haest"));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The working directory: relative paths resolve against it, and
    /// instruction files and skills are looked for from it upward.
    pub cwd: PathBuf,
    /// The human's home: where `~/.agents` and `~/.claude` are, and where
    /// [`Settings::bare`] puts `data_dir` and `config_dir`.
    pub home: PathBuf,
    /// The model, as `provider:model` (`openrouter:anthropic/claude-sonnet-5.5`).
    /// Bare: [`DEFAULT_MODEL`].
    pub model: String,
    /// Reasoning effort; None sends none, and the model uses its own default.
    pub effort: Option<Effort>,
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
    /// GenerateImage's default model, an OpenRouter model id.
    pub image_model: String,
    /// The other models a GenerateImage call may pick; None = haest's list
    /// ([`DEFAULT_IMAGE_MODELS`]).
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
    /// The built-in approval mod: asks before a tool changes anything. Bare: on.
    pub approvals: bool,
    /// The spend, token, step and time caps.
    pub limits: Limits,
    /// Turns in a row a background job or an agent may start with no human
    /// message; past it haest holds them. 0 = no cap.
    pub max_agent_turns: u32,
    /// The caps on the subagent tree: depth, how many at once, and each one's budget.
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
    /// search, no image tool, no mods; approvals on, the sandbox workspace,
    /// [`DEFAULT_MODEL`], MCP tools behind ToolSearch, and the caps in
    /// [`Limits`] at their bare values (2 USD and 100 model calls a turn, 20
    /// USD a conversation, ...). `data_dir` is `<home>/.local/share/haest` and
    /// `config_dir` `<home>/.config/haest`. Neither path is checked or created.
    ///
    /// ```
    /// use haest_settings::{Settings, WordSet};
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
            model: DEFAULT_MODEL.into(),
            effort: None,
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
            image_model: DEFAULT_IMAGE_MODEL.into(),
            image_models: None,
            deferred_tools: DeferMode::Proxy,
            workspace: WorkspaceMode::Sandbox,
            mods: false,
            project_mods: false,
            approvals: true,
            limits: Limits {
                max_turn_usd: 2.0,
                max_turn_tokens: None,
                max_request_tokens: None,
                max_turn_steps: 100,
                max_turn_minutes: 60.0,
                max_repeats: 6,
                max_conversation_usd: 20.0,
                max_conversation_tokens: 20_000_000,
                context_window: None,
                compact_at: 400_000,
            },
            max_agent_turns: 30,
            subagents: SubagentCaps::default(),
            data_dir: home.join(".local/share/haest"),
            config_dir: home.join(".config/haest"),
            debug: false,
            home,
        }
    }

    /// The model's context window in tokens: [`Limits::context_window`] when
    /// set above 1000, else `catalog_window` (the catalog row's, for a model
    /// the catalog knows), else 200K.
    ///
    /// ```
    /// use haest_settings::Settings;
    ///
    /// let mut s = Settings::bare("/w", "/h");
    /// assert_eq!(s.window(None), 200_000);
    /// assert_eq!(s.window(Some(1_000_000)), 1_000_000);
    /// s.limits.context_window = Some(50_000);
    /// assert_eq!(s.window(Some(1_000_000)), 50_000);
    /// ```
    pub fn window(&self, catalog_window: Option<u64>) -> u64 {
        let set = self.limits.context_window.filter(|n| *n > 1000);
        set.or(catalog_window).unwrap_or(200_000)
    }

    /// The loop's per-turn caps for a model with this `window` (from
    /// [`Settings::window`]).
    ///
    /// An unset token cap follows the window: `max_turn_tokens` is 10x it,
    /// `max_request_tokens` 90% of it. One set to `Some(0)` stays 0, no cap.
    /// The wall clock comes out in milliseconds.
    pub fn limits_for(&self, window: u64) -> TurnLimits {
        let l = &self.limits;
        TurnLimits {
            max_turn_usd: l.max_turn_usd,
            max_turn_tokens: l.max_turn_tokens.unwrap_or(window * 10),
            max_request_tokens: l.max_request_tokens.unwrap_or(window * 9 / 10),
            max_turn_steps: l.max_turn_steps,
            max_turn_ms: (l.max_turn_minutes * 60_000.0) as u64,
            max_repeats: l.max_repeats,
        }
    }

    /// The caps that span the whole conversation, for haest-core's ledger:
    /// `max_conversation_usd` and `max_conversation_tokens`.
    pub fn ledger(&self) -> LedgerCaps {
        LedgerCaps {
            max_usd: self.limits.max_conversation_usd,
            max_unpriced_tokens: self.limits.max_conversation_tokens,
        }
    }

    /// The Code tool's setting in one value: None when `code` is off, else the
    /// extra tool names it may call (`code_tools`, possibly empty).
    pub fn code_tools(&self) -> Option<Vec<String>> {
        self.code.then(|| self.code_tools.clone())
    }

    /// The web search a model call carries, or None when `web_search` is off.
    /// The result caps are haest's ([`WebSearchConfig::new`]: 5 a search, 15 a
    /// model call).
    pub fn web_search_config(&self) -> Option<WebSearchConfig> {
        self.web_search.map(WebSearchConfig::new)
    }

    /// Every model GenerateImage may pick: `image_model` first, then
    /// `image_models` (or haest's list) without it, so each appears once.
    ///
    /// ```
    /// use haest_settings::Settings;
    ///
    /// let s = Settings {
    ///     image_model: "b/two".into(),
    ///     image_models: Some(vec!["a/one".into(), "b/two".into()]),
    ///     ..Settings::bare("/w", "/h")
    /// };
    /// assert_eq!(s.image_models(), ["b/two", "a/one"]);
    /// ```
    pub fn image_models(&self) -> Vec<String> {
        let others = match &self.image_models {
            Some(list) => list.clone(),
            None => DEFAULT_IMAGE_MODELS.map(String::from).to_vec(),
        };
        let model = &self.image_model;
        std::iter::once(model.clone())
            .chain(others.into_iter().filter(|m| m != model))
            .collect()
    }

    /// Where one session's spilled tool output goes:
    /// `<data_dir>/spill/<session_id>`. A tool result too big for the context
    /// is written there and the model gets its path instead.
    pub fn spill_dir(&self, session_id: &str) -> PathBuf {
        self.data_dir.join("spill").join(session_id)
    }
}
