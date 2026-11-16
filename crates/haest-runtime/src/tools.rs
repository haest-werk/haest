//! The tools a conversation offers: [`own_tools`], [`WrappedTools`], [`MainTools`].

use std::sync::{Arc, Mutex};

use haest_settings::{BuiltinTools, Settings, WordSet};

use crate::haest::{
    KeySource,
    code::{CodeTool, code_tool_names},
    core::__private::openrouter_base_url,
    core::fs::Fs,
    core::skills::Skill,
    core::task_board::TaskBoard,
    core::tool::{Tool, ToolSource},
    mcp::McpHub,
    mods::{ModTools, Mods},
    tools::{
        AgentRef, BashOptions, BuiltinOptions, GenerateImageOptions, GenerateImageTool, JobHub,
        SkillToolOptions, SubagentHub, builtin_tools_where,
    },
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
/// ([`jobs_for`](crate::jobs_for)), and GenerateImage is there when
/// `image` is ([`image_options`]).
pub fn own_tools(
    picked: &BuiltinTools,
    board: Arc<TaskBoard>,
    skills: Arc<Mutex<Vec<Skill>>>,
    host_fs: Arc<dyn Fs>,
    jobs: Option<Arc<JobHub>>,
    image: Option<GenerateImageOptions>,
) -> Vec<Arc<dyn Tool>> {
    let opts = BuiltinOptions {
        bash: BashOptions {
            jobs,
            ..Default::default()
        },
        tasks: Some(board),
        skill: Some(SkillToolOptions::new(
            move || skills.lock().unwrap().clone(),
            host_fs,
        )),
        ..Default::default()
    };
    let mut tools = builtin_tools_where(|w| picked.has(w), opts)
        .expect("every option a built-in needs is given");
    if let Some(opts) = image {
        tools.push(Arc::new(GenerateImageTool::new(opts)));
    }
    tools
}

/// `image` is on, and the host answers no `OPENROUTER_API_KEY`:
/// GenerateImage has no key to call OpenRouter with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("image is on, but the host has no OPENROUTER_API_KEY: no GenerateImage")]
pub struct NoImageKey;

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
pub fn image_options(
    s: &Settings,
    keys: &dyn KeySource,
) -> Result<Option<GenerateImageOptions>, NoImageKey> {
    if !s.image {
        return Ok(None);
    }
    let key = keys.key("OPENROUTER_API_KEY").ok_or(NoImageKey)?;
    let opts = GenerateImageOptions {
        base_url: openrouter_base_url(keys),
        ..GenerateImageOptions::new(key)
    };
    // Unset, each is the tool's own.
    let model = s.image_model.clone().unwrap_or_else(|| opts.model.clone());
    let others = s
        .image_models
        .clone()
        .unwrap_or_else(|| opts.models.clone());
    Ok(Some(opts.with_models(model, others)))
}

/// The conversation's own tools and the MCP servers' as they are now (a server
/// that reconnects is picked up on the next call).
struct BaseTools {
    own: Vec<Arc<dyn Tool>>,
    mcp: Arc<McpHub>,
}

impl ToolSource for BaseTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut all = self.own.clone();
        all.extend(self.mcp.tools());
        all
    }
}

/// Every tool but the agent tools, inside the mods' hooks (TS `baseTools()`).
///
/// A [`ToolSource`]: the conversation's own tools, then every connected MCP
/// server's tools as they are at each call, then the Code tool when `code` is
/// on, all wrapped in the mods' `tool.before` / `tool.after` hooks. Every
/// subagent starts from this list.
pub struct WrappedTools {
    base: Arc<BaseTools>,
    mods: Arc<Mods>,
    /// The extra names Code may call (`code_tools`); None = no Code tool.
    code: Option<Vec<String>>,
}

impl WrappedTools {
    /// `own` and `mcp`'s tools inside `mods`' hooks. `code`: None = no Code
    /// tool; Some(extra) = a Code tool that may call the file tools, the
    /// read-only MCP tools, and the tools `extra` names (the `code_tools`
    /// setting: exact names, or `prefix*`).
    pub fn new(
        own: Vec<Arc<dyn Tool>>,
        mcp: Arc<McpHub>,
        mods: Arc<Mods>,
        code: Option<Vec<String>>,
    ) -> Self {
        Self {
            base: Arc::new(BaseTools { own, mcp }),
            mods,
            code,
        }
    }
}

impl ToolSource for WrappedTools {
    /// Code calls the WRAPPED siblings, read per run, so every call it makes passes the hooks too.
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base.tools();
        if let Some(extra) = &self.code {
            let names = code_tool_names(&tools, extra);
            let callable = Arc::new(ModTools::new(self.base.clone(), self.mods.clone()));
            tools.push(Arc::new(CodeTool::new(names, callable)));
        }
        ModTools::wrap(&self.mods, tools)
    }
}

/// Main's list (TS `tools()`): the wrapped tools and main's agent tools
/// (none when `agents` is off). The build puts it behind haest-tools'
/// `DeferredTools`, which defers as the `deferred_tools` setting says.
pub struct MainTools {
    wrapped: Arc<WrappedTools>,
    hub: Option<Arc<SubagentHub>>,
}

impl MainTools {
    /// Main's list over `wrapped`, with `hub`'s agent tools for main (None =
    /// no agent tools).
    pub fn new(wrapped: Arc<WrappedTools>, hub: Option<Arc<SubagentHub>>) -> Self {
        Self { wrapped, hub }
    }
}

impl ToolSource for MainTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut all = self.wrapped.tools();
        if let Some(hub) = &self.hub {
            all.extend(hub.tools_for(&AgentRef::main()));
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::haest::tools::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS};

    fn names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
        tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// The own tools `picked` names, with no jobs and no image.
    fn build(picked: &BuiltinTools) -> Vec<Arc<dyn Tool>> {
        let fs: Arc<dyn Fs> = Arc::new(crate::haest::MemFs::new());
        let skills = Arc::new(Mutex::new(Vec::new()));
        own_tools(picked, Arc::default(), skills, fs, None, None)
    }

    #[test]
    fn every_tools_word_picks_a_built_in_and_every_built_in_has_a_word() {
        for word in BuiltinTools::WORDS {
            let one = BuiltinTools::from_words([*word]).unwrap();
            assert!(!build(&one).is_empty(), "{word} picks nothing");
        }
        for tool in build(&BuiltinTools::ALL) {
            let word = crate::haest::tools::builtin_word(tool.name());
            assert!(
                word.is_some_and(|w| BuiltinTools::WORDS.contains(&w)),
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
        assert_eq!(
            (opts.api_key.as_str(), opts.base_url.as_str()),
            ("k", crate::haest::core::__private::OPENROUTER_URL)
        );
        assert_eq!(opts.model, DEFAULT_IMAGE_MODEL);
        assert_eq!(opts.models, DEFAULT_IMAGE_MODELS);
        let local = keys(&[
            ("OPENROUTER_API_KEY", "k"),
            ("OPENROUTER_BASE_URL", "http://l/v1"),
        ]);
        let opts = image_options(&on, &local).unwrap().unwrap();
        assert_eq!(opts.base_url, "http://l/v1");
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
            image_options(&s, &key).unwrap().unwrap().models
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
