//! The system prompt: [`RuntimePrompt`] and [`AgentPrompt`].

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use haest_core::{
    SystemPrompt, ToolRef, ToolSourceRef, Workspace,
    context::{
        InstructionOptions, PromptInputs, build_system_prompt, current_git_branch,
        find_instruction_files, tool_guide,
    },
    fs::FsRef,
    now_ms,
    session::iso_utc,
    skills::{SkillOptions, Skills},
};
use haest_settings::Settings;
use haest_tools::touches_workspace;

use crate::RuntimeHooks;

/// Which instruction files the `context` setting asks for (AGENTS.md,
/// CLAUDE.md and their kin), looked for from `cwd` and `home`: what
/// `find_instruction_files` takes.
pub(crate) fn instruction_options(s: &Settings) -> InstructionOptions {
    InstructionOptions {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        claude_dir: claude_dir(s),
        sources: s.context,
    }
}

/// Which skill dirs the `skills` setting asks for, under `cwd` and `home`,
/// with `~/.claude` as Claude Code's user dir (its skills and its plugin
/// list): what `SkillScanRoots::find` takes.
pub(crate) fn skill_options(s: &Settings) -> SkillOptions {
    SkillOptions {
        cwd: s.cwd.clone(),
        home: s.home.clone(),
        claude_dir: claude_dir(s),
        sources: s.skills,
    }
}

/// Claude Code's user dir: `~/.claude`.
fn claude_dir(s: &Settings) -> PathBuf {
    s.home.join(".claude")
}

/// The system prompt's parts that every agent of the conversation shares, and
/// how to build one for a given model and tool list ([`build`](Self::build)).
///
/// The prompt is built before every model call: instruction files are read
/// fresh (an edited AGENTS.md takes effect on the next call), the tool names
/// are the list this call advertises; skills are the list found at start or at
/// the last /skills, in the prompt only with `skill_catalog`. Main and every
/// subagent build it the same way, each naming its OWN model and tools.
///
/// Wrap it in an [`AgentPrompt`] to give an agent its prompt.
pub(crate) struct RuntimePrompt {
    /// Where the instruction files are read from.
    fs: FsRef,
    /// Its settings say which instruction files to read (`context`) and
    /// whether the skill list goes in (`skill_catalog`).
    hooks: RuntimeHooks,
    /// The skills found; listed in the prompt with `skill_catalog` on.
    skills: Skills,
    /// Everything but the instructions, skills, tools and model, fixed for the process.
    base: PromptInputs,
    /// The host's template, rendered in place of the built-in prompt.
    #[cfg(feature = "prompt")]
    template: Option<Template>,
}

/// The host's template, and the last render error the human was told.
#[cfg(feature = "prompt")]
struct Template {
    template: haest_prompt::PromptTemplate,
    warned: std::sync::Mutex<String>,
}

impl RuntimePrompt {
    /// The parts fixed for the process; the model and tools are per agent.
    ///
    /// Reads them once, now: the identity text (`system_prompt`), the working
    /// dir, the platform, today's date, the git branch, the workspace's own
    /// note, `append_system_prompt`.
    pub(crate) async fn new(
        hooks: &RuntimeHooks,
        fs: &FsRef,
        skills: &Skills,
        workspace: &dyn Workspace,
    ) -> Self {
        let s = hooks.settings();
        let note = workspace.prompt_note();
        let base = PromptInputs {
            identity: s.system_prompt.clone(),
            cwd: s.cwd.to_string_lossy().into_owned(),
            platform: platform(),
            date: iso_utc(now_ms())[..10].to_string(),
            git_branch: current_git_branch(fs.as_ref(), &s.cwd).await,
            workspace_note: (!note.trim().is_empty()).then_some(note),
            append_system_prompt: s.append_system_prompt.clone(),
            ..Default::default()
        };
        Self {
            fs: fs.clone(),
            hooks: hooks.clone(),
            skills: skills.clone(),
            base,
            #[cfg(feature = "prompt")]
            template: None,
        }
    }

    /// The same, rendering `template` (None: the built-in prompt).
    #[cfg(feature = "prompt")]
    pub(crate) fn with_template(mut self, template: Option<haest_prompt::PromptTemplate>) -> Self {
        self.template = template.map(|template| Template {
            template,
            warned: std::sync::Mutex::default(),
        });
        self
    }

    /// The host's template rendered for `p` and `tools`; None without one,
    /// or when the render failed (the human is told each new error).
    #[cfg(feature = "prompt")]
    fn render(&self, p: &PromptInputs, tools: &[ToolRef]) -> Option<String> {
        use haest_core::catalog::ModelFacts;

        let t = self.template.as_ref()?;
        let model = self.hooks.model_info(&p.model_spec);
        let context = haest_prompt::PromptContext::new(p, tools, model.as_ref());
        let err = match t.template.render(&context) {
            Ok(text) => return Some(text),
            Err(e) => e.to_string(),
        };
        let fresh = {
            // Held only to compare and store a string: nothing in it panics.
            let mut warned = t
                .warned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let fresh = *warned != err;
            if fresh {
                warned.clone_from(&err);
            }
            fresh
        };
        if fresh {
            self.hooks.warn(&format!(
                "The system prompt template failed ({err}); haest's built-in prompt is used until it renders."
            ));
        }
        None
    }

    /// The system prompt for an agent on `model` offered `tools`: the fixed
    /// parts, the tools' own guide lines, the workspace's note when a tool
    /// can touch it, the instruction files read again now, and the skill list
    /// with `skill_catalog` on. With a template, the template's rendering of
    /// them.
    pub(crate) async fn build(&self, model: String, tools: &[ToolRef]) -> String {
        let s = self.hooks.settings();
        let mut p = self.base.clone();
        p.model_spec = model;
        p.tool_guide = tool_guide(tools);
        p.tool_names = tools.iter().map(|t| t.name().to_string()).collect();
        if !touches_workspace(&p.tool_names) {
            p.workspace_note = None;
        }
        p.instructions = find_instruction_files(self.fs.as_ref(), &instruction_options(s)).await;
        if s.skill_catalog {
            p.skills = self.skills.list();
        }
        #[cfg(feature = "prompt")]
        if let Some(text) = self.render(&p, tools) {
            return text;
        }
        build_system_prompt(&p)
    }
}

/// One agent's prompt: its model (None = the conversation's current one) and
/// the tools its next call advertises. Wrap it in the mods' `ModSystemPrompt`.
///
/// It is a [`SystemPrompt`]: the loop asks it before every model call.
pub(crate) struct AgentPrompt {
    /// The parts every agent shares.
    pub(crate) prompt: Arc<RuntimePrompt>,
    /// The model the prompt names; None = the conversation's current one
    /// ([`RuntimeHooks::model_spec`]), as for main.
    pub(crate) model_spec: Option<String>,
    /// The agent's tools: the prompt names the ones its next call offers.
    pub(crate) tools: ToolSourceRef,
}

#[async_trait]
impl SystemPrompt for AgentPrompt {
    async fn system_prompt(&self) -> String {
        let model = self
            .model_spec
            .clone()
            .unwrap_or_else(|| self.prompt.hooks.model_spec());
        self.prompt.build(model, &self.tools.tools()).await
    }
}

/// The platform the system prompt names: Rust's `std::env::consts::OS`, with
/// `macos` as `darwin`, the name most models' training data uses.
fn platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".into(),
        os => os.into(),
    }
}

#[cfg(test)]
mod tests {
    use haest_core::fs::MemFs;
    use haest_tools::{BuiltinOptions, builtin_tools};

    use super::*;
    use crate::testkit::hooks;

    #[tokio::test]
    async fn the_tool_guide_and_the_workspace_note_go_only_where_the_tools_reach() {
        const NOTE: &str = "SANDBOX LIMITS";
        let (hooks, _) = hooks(Settings::bare(PathBuf::from("/w"), PathBuf::from("/h")));
        let prompt = RuntimePrompt {
            fs: FsRef::new(MemFs::new()),
            hooks,
            skills: Skills::new(MemFs::new()),
            base: PromptInputs {
                workspace_note: Some(NOTE.into()),
                ..Default::default()
            },
            #[cfg(feature = "prompt")]
            template: None,
        };
        let all = builtin_tools(BuiltinOptions::default());
        let only = |name: &str| -> Vec<ToolRef> {
            all.iter().filter(|t| t.name() == name).cloned().collect()
        };
        let read = only("Read");
        let guide = tool_guide(&read).unwrap();
        let with_files = prompt.build("m".into(), &read).await;
        assert!(with_files.contains(&guide) && with_files.contains(NOTE));
        let web = prompt.build("m".into(), &only("WebFetch")).await;
        assert!(!web.contains("Tool discipline") && !web.contains(NOTE));
    }

    #[cfg(feature = "prompt")]
    #[tokio::test]
    async fn a_template_renders_the_prompt_and_a_failed_render_falls_back_saying_so_once() {
        use haest_prompt::PromptTemplate;

        let (hooks, ui) = hooks(Settings::bare(PathBuf::from("/w"), PathBuf::from("/h")));
        let fs = FsRef::new(MemFs::new());
        let skills = Skills::new(MemFs::new());
        let workspace = haest_core::ShutWorkspace;
        let new = || RuntimePrompt::new(&hooks, &fs, &skills, &workspace);
        let opus = "anthropic:claude-opus-5-5";

        // The model's facts come from the built-in catalog.
        let source = "{{ model.family }} {{ model.tier }} {{ model.generation }} in {{ cwd }}";
        let template = PromptTemplate::new(source).unwrap();
        let prompt = new().await.with_template(Some(template));
        assert_eq!(
            prompt.build(opus.into(), &[]).await,
            "anthropic opus 5.5 in /w"
        );

        let broken = PromptTemplate::new("{{ nobody_set_this }}").unwrap();
        let prompt = new().await.with_template(Some(broken));
        let built = new().await.build(opus.into(), &[]).await;
        assert_eq!(prompt.build(opus.into(), &[]).await, built);
        assert_eq!(prompt.build(opus.into(), &[]).await, built);
        let warned = ui.warnings();
        assert_eq!(warned.len(), 1, "{warned:?}");
        assert!(warned[0].contains("system prompt template failed"));
    }
}
