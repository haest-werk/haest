//! The tools a conversation offers: [`own_tools`], [`WrappedTools`], [`MainTools`].

use std::sync::Arc;

use haest_code::{CodeTool, code_tool_names};
use haest_core::{
    __private::openrouter_base_url, KeySource, Tools, skills::Skills, task_board::TaskBoard,
    tool::ToolSource,
};
use haest_mcp::McpHub;
use haest_mods::{ModTools, Mods};
use haest_settings::{BuiltinTools, Settings};
use haest_tools::{
    AgentRef, BashOptions, BuiltinOptions, FILE_TOOLS, GenerateImageOptions, GenerateImageTool,
    JobHub, SkillToolOptions, SubagentHub, builtin_tools_where,
};

/// Everything but MCP: the built-ins, task tools, Skill (over the host's Fs:
/// a user skill lives outside any sandbox mount) and GenerateImage.
///
/// The first part of the tools a conversation offers, composed as the TS
/// haest does (`host/runtime.ts`), each only when its setting turns it on: the built-ins
/// `tools` names (Bash with background jobs in the real workspace), the task
/// list, Skill, GenerateImage (`image`), and every connected MCP server's tools
/// (`mcp`); the Code tool over them (`code`); all of it inside the mods'
/// `tool.before` / `tool.after` hooks -- [`WrappedTools`], the list every
/// subagent starts from. Main's list ([`MainTools`]) adds main's agent tools
/// (`agents`), NOT mod-wrapped; the build defers the MCP schemas behind
/// ToolSearch (`deferred_tools`) so a big server does not ride on every call.
///
/// `picked` is the `tools` setting: only the built-ins it names are made.
/// The task tools work on `board`, Skill offers what `skills` holds when it is
/// called, Bash leaves background jobs to `jobs` when there is a hub
/// ([`jobs_for`](crate::world::jobs_for)), and GenerateImage is there when
/// `image` is ([`image_options`]).
pub(crate) fn own_tools(
    picked: &BuiltinTools,
    board: TaskBoard,
    skills: Skills,
    jobs: Option<JobHub>,
    image: Option<GenerateImageOptions>,
) -> Tools {
    let bash = jobs
        .into_iter()
        .fold(BashOptions::default(), BashOptions::with_jobs);
    let opts = BuiltinOptions::new()
        .with_bash(bash)
        .with_tasks(board)
        .with_skill(SkillToolOptions::new(skills));
    let mut tools = builtin_tools_where(|t| picked.has(t.as_str()), opts)
        .expect("every option a built-in needs is given");
    if let Some(opts) = image {
        tools.push(GenerateImageTool::new(opts));
    }
    tools
}

/// `image` is on, and the host answers no `OPENROUTER_API_KEY`:
/// GenerateImage has no key to call OpenRouter with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("image is on, but the host has no OPENROUTER_API_KEY: no GenerateImage")]
pub(crate) struct NoImageKey;

/// GenerateImage's options: `image` on, over OpenRouter with the key the host
/// answers for `OPENROUTER_API_KEY`. On without one: why there is none.
///
/// None with `image` off. The endpoint is `OPENROUTER_BASE_URL` when the host
/// answers one, and the models are the `image_model` and `image_models`
/// settings'.
///
/// # Errors
///
/// [`NoImageKey`] with `image` on and no `OPENROUTER_API_KEY`.
/// [`build`](crate::build) warns and goes on without GenerateImage.
pub(crate) fn image_options(
    s: &Settings,
    keys: &dyn KeySource,
) -> Result<Option<GenerateImageOptions>, NoImageKey> {
    if !s.image {
        return Ok(None);
    }
    let key = keys.key("OPENROUTER_API_KEY").ok_or(NoImageKey)?;
    let opts = GenerateImageOptions::new(key).with_base_url(openrouter_base_url(keys));
    // Unset, each is the tool's own.
    let model = s
        .image_model
        .clone()
        .unwrap_or_else(|| opts.model().to_string());
    let others = s
        .image_models
        .clone()
        .unwrap_or_else(|| opts.models().to_vec());
    Ok(Some(opts.with_models(model, others)))
}

/// The conversation's own tools and the MCP servers' as they are now (a server
/// that reconnects is picked up on the next call).
struct BaseTools {
    own: Tools,
    mcp: McpHub,
}

#[warn(clippy::missing_trait_methods)]
impl ToolSource for BaseTools {
    fn tools(&self) -> Tools {
        let mut all = self.own.tools();
        all.extend(self.mcp.tools());
        all
    }
    fn callable(&self) -> Tools {
        let mut all = self.own.callable();
        all.extend(self.mcp.callable());
        all
    }
}

/// Every tool but the agent tools, inside the mods' hooks (TS `baseTools()`).
///
/// A [`ToolSource`]: the conversation's own tools, then every connected MCP
/// server's tools as they are at each call, then the Code tool when `code` is
/// on, all wrapped in the mods' `tool.before` / `tool.after` hooks. Every
/// subagent starts from this list.
pub(crate) struct WrappedTools {
    base: Arc<BaseTools>,
    mods: Mods,
    /// The names Code may call: the file tools and `code_tools`; None = no
    /// Code tool.
    code: Option<Vec<String>>,
}

impl WrappedTools {
    /// `own` and `mcp`'s tools inside `mods`' hooks. `code`: None = no Code
    /// tool; Some(extra) = a Code tool that may call the file tools, the
    /// read-only MCP tools, and the tools `extra` names (the `code_tools`
    /// setting: exact names, or `prefix*`).
    pub(crate) fn new(own: Tools, mcp: McpHub, mods: Mods, code: Option<Vec<String>>) -> Self {
        let files = FILE_TOOLS.map(String::from);
        Self {
            base: Arc::new(BaseTools { own, mcp }),
            mods,
            code: code.map(|extra| files.into_iter().chain(extra).collect()),
        }
    }

    /// `tools` (the base's), then the Code tool when it is on, all inside
    /// the mods' hooks, less the tools a mod hides. Code calls the WRAPPED
    /// siblings, read per run, so every call it makes passes the hooks too,
    /// and it neither names nor reaches a hidden one.
    fn wrap(&self, mut tools: Tools) -> Tools {
        if let Some(allowed) = &self.code {
            let hidden = self.mods.disabled_tools();
            let mut names = code_tool_names(&tools, allowed);
            names.retain(|name| !hidden.contains(name));
            let callable = ModTools::new(self.base.clone(), self.mods.clone());
            tools.push(CodeTool::new(names, callable));
        }
        ModTools::wrap(&self.mods, tools)
    }
}

#[warn(clippy::missing_trait_methods)]
impl ToolSource for WrappedTools {
    fn tools(&self) -> Tools {
        self.wrap(self.base.tools())
    }
    fn callable(&self) -> Tools {
        self.wrap(self.base.callable())
    }
}

/// Main's list (TS `tools()`): the wrapped tools and main's agent tools
/// (none when `agents` is off). The build puts it behind haest-tools'
/// `DeferredTools`, which defers as the `deferred_tools` setting says.
pub(crate) struct MainTools {
    wrapped: Arc<WrappedTools>,
    hub: Option<SubagentHub>,
}

impl MainTools {
    /// Main's list over `wrapped`, with `hub`'s agent tools for main (None =
    /// no agent tools).
    pub(crate) fn new(wrapped: Arc<WrappedTools>, hub: Option<SubagentHub>) -> Self {
        Self { wrapped, hub }
    }

    /// `wrapped`, then main's agent tools, less those a mod hides (they are
    /// not inside the mods' hooks, but a mod hides any tool).
    fn with_agents(&self, mut wrapped: Tools) -> Tools {
        if let Some(hub) = &self.hub {
            let hidden = self.wrapped.mods.disabled_tools();
            let agents = hub.tools_for(&AgentRef::main());
            wrapped.extend(
                agents
                    .into_iter()
                    .filter(|t| !hidden.iter().any(|h| h == t.name())),
            );
        }
        wrapped
    }
}

#[warn(clippy::missing_trait_methods)]
impl ToolSource for MainTools {
    fn tools(&self) -> Tools {
        self.with_agents(self.wrapped.tools())
    }
    fn callable(&self) -> Tools {
        self.with_agents(self.wrapped.callable())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use haest_core::ToolRef;
    use haest_tools::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS};

    use super::*;

    fn names(tools: &[ToolRef]) -> Vec<String> {
        tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// The own tools `picked` names, with no jobs and no image.
    fn build(picked: &BuiltinTools) -> Tools {
        let skills = Skills::new(haest_core::MemFs::new());
        own_tools(picked, TaskBoard::default(), skills, None, None)
    }

    #[test]
    fn every_tools_word_picks_a_built_in_and_every_built_in_has_a_word() {
        for word in BuiltinTools::WORDS {
            let one = BuiltinTools::from_words([*word]).unwrap();
            assert!(!build(&one).is_empty(), "{word} picks nothing");
        }
        for tool in build(&BuiltinTools::ALL) {
            let builtin = haest_tools::BuiltinTool::of(tool.name());
            assert!(
                builtin.is_some_and(|b| BuiltinTools::WORDS.contains(&b.as_str())),
                "{}",
                tool.name()
            );
        }
    }

    #[test]
    fn task_and_skill_tools_follow_their_switches() {
        assert!(build(&BuiltinTools::NONE).is_empty());
        let with = names(&build(&BuiltinTools {
            tasks: true,
            skill: true,
            ..BuiltinTools::NONE
        }));
        assert!(
            with.contains(&"Skill".to_string()) && with.len() > 1,
            "{with:?}"
        );
        assert!(!with.contains(&"Read".to_string()));
    }

    #[test]
    fn generate_image_needs_the_switch_and_a_key_the_host_answers_and_says_when_it_has_none() {
        let on = Settings {
            image: true,
            ..crate::testkit::settings()
        };
        let keys = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let with_key = keys(&[("OPENROUTER_API_KEY", "k")]);
        let off = crate::testkit::settings();
        assert!(image_options(&off, &with_key).unwrap().is_none());
        assert_eq!(image_options(&on, &keys(&[])).unwrap_err(), NoImageKey);
        let opts = image_options(&on, &with_key).unwrap().unwrap();
        assert_eq!(opts.base_url(), haest_core::__private::OPENROUTER_URL);
        assert_eq!(opts.model(), DEFAULT_IMAGE_MODEL);
        assert_eq!(opts.models(), DEFAULT_IMAGE_MODELS);
        let local = keys(&[
            ("OPENROUTER_API_KEY", "k"),
            ("OPENROUTER_BASE_URL", "http://l/v1"),
        ]);
        let opts = image_options(&on, &local).unwrap().unwrap();
        assert_eq!(opts.base_url(), "http://l/v1");
    }

    #[test]
    fn the_image_settings_put_the_chosen_model_first_once() {
        let key = HashMap::from([("OPENROUTER_API_KEY".to_string(), "k".to_string())]);
        let with = |model: Option<&str>, others: Option<Vec<&str>>| {
            let s = Settings {
                image: true,
                image_model: model.map(String::from),
                image_models: others.map(|o| o.into_iter().map(String::from).collect()),
                ..crate::testkit::settings()
            };
            image_options(&s, &key).unwrap().unwrap().models().to_vec()
        };
        assert_eq!(with(None, None), DEFAULT_IMAGE_MODELS);
        let chosen = with(Some("x/pic"), Some(vec!["a/one", "x/pic", "b/two"]));
        assert_eq!(chosen, ["x/pic", "a/one", "b/two"]);
        assert_eq!(with(Some("x/pic"), Some(vec![])), ["x/pic"]);
        let first = with(Some("x/pic"), None);
        assert_eq!(first[0], "x/pic");
        assert_eq!(first.len(), DEFAULT_IMAGE_MODELS.len() + 1);
    }
}
