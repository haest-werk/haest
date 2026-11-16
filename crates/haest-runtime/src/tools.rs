//! The tools a conversation offers, composed as the TS haest does
//! (`host/runtime.ts`), each only when its setting turns it on: the built-ins
//! `tools` names (Bash with background jobs in the real workspace), the task
//! list, Skill, GenerateImage (`image`), and every connected MCP server's tools
//! (`mcp`); the Code tool over them (`code`); all of it inside the mods'
//! `tool.before` / `tool.after` hooks -- [`WrappedTools`], the list every
//! subagent starts from. Main's list ([`MainTools`]) adds main's agent tools
//! (`agents`), NOT mod-wrapped, and defers the MCP schemas behind ToolSearch
//! (`deferred_tools`) so a big server does not ride on every call.

use std::sync::{Arc, Mutex};

use haest_settings::{BuiltinTools, Settings, WordSet};

use crate::haest::{
    KeySource,
    code::{CodeTool, code_tool_names},
    deferred_tools::{DeferMode, ToolDeferral},
    fs::Fs,
    handed::openrouter_base_url,
    mcp::McpHub,
    mods::{ModTools, Mods},
    skills::Skill,
    task_board::TaskBoard,
    tool::{Tool, ToolSource},
    tools::{
        BashOptions, BuiltinOptions, ImageTool, ImageToolOptions, JobHub, SkillToolOptions,
        agents::{AgentRef, SubagentHub},
        builtin_tools_where,
        tool_search::deferral_tools,
    },
};

/// Everything but MCP: the built-ins, task tools, Skill (over the host's Fs:
/// a user skill lives outside any sandbox mount) and GenerateImage.
pub fn own_tools(
    picked: &BuiltinTools,
    board: Arc<TaskBoard>,
    skills: Arc<Mutex<Vec<Skill>>>,
    host_fs: Arc<dyn Fs>,
    jobs: Option<Arc<JobHub>>,
    image: Option<ImageToolOptions>,
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
        tools.push(Arc::new(ImageTool::new(opts)));
    }
    tools
}

/// GenerateImage's options: `image` on, over OpenRouter with the key the host
/// answers for `OPENROUTER_API_KEY`. On without one: why there is none.
pub fn image_options(
    s: &Settings,
    keys: &dyn KeySource,
) -> Result<Option<ImageToolOptions>, String> {
    if !s.image {
        return Ok(None);
    }
    let key = keys.key("OPENROUTER_API_KEY").ok_or(
        "image is on, but the host has no OPENROUTER_API_KEY: no GenerateImage.".to_string(),
    )?;
    let opts = ImageToolOptions {
        base_url: openrouter_base_url(keys),
        ..ImageToolOptions::new(key)
    };
    Ok(Some(
        opts.with_models(s.image_model.clone(), s.image_models()),
    ))
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
pub struct WrappedTools {
    base: Arc<BaseTools>,
    mods: Arc<Mods>,
    /// The extra names Code may call (`code_tools`); None = no Code tool.
    code: Option<Vec<String>>,
}

impl WrappedTools {
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
/// (none when `agents` is off), deferred.
pub struct MainTools {
    wrapped: Arc<WrappedTools>,
    hub: Option<Arc<SubagentHub>>,
    deferral: Arc<ToolDeferral>,
}

impl MainTools {
    pub fn new(wrapped: Arc<WrappedTools>, hub: Option<Arc<SubagentHub>>, mode: DeferMode) -> Self {
        Self {
            wrapped,
            hub,
            deferral: Arc::new(ToolDeferral::new(mode)),
        }
    }

    fn all(&self) -> Vec<Arc<dyn Tool>> {
        let mut all = self.wrapped.tools();
        if let Some(hub) = &self.hub {
            all.extend(hub.tools_for(&AgentRef::main()));
        }
        all
    }
}

impl ToolSource for MainTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let d = &self.deferral;
        d.advertise(&self.all(), deferral_tools(d.clone()))
    }
    fn callable(&self) -> Vec<Arc<dyn Tool>> {
        let d = &self.deferral;
        d.callable(&self.all(), deferral_tools(d.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

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
        let none = image_options(&on, &keys(&[])).unwrap_err();
        assert!(none.contains("OPENROUTER_API_KEY"), "{none}");
        let opts = image_options(&on, &with_key).unwrap().unwrap();
        assert_eq!(
            (opts.api_key.as_str(), opts.base_url.as_str()),
            ("k", crate::haest::handed::OPENROUTER_URL)
        );
        assert_eq!(opts.models, on.image_models());
        let local = keys(&[
            ("OPENROUTER_API_KEY", "k"),
            ("OPENROUTER_BASE_URL", "http://l/v1"),
        ]);
        let opts = image_options(&on, &local).unwrap().unwrap();
        assert_eq!(opts.base_url, "http://l/v1");
    }
}
