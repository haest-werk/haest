//! The tools a CLI conversation offers, composed as the TS kern does
//! (`host/runtime.ts`): the built-ins (Bash with background jobs in the real
//! workspace), the task list, Skill, GenerateImage (with an OpenRouter key) and
//! every connected MCP server's tools; the Code tool over them; all of it
//! inside the mods' `tool.before` / `tool.after` hooks -- [`WrappedTools`], the
//! list every subagent starts from. Main's list ([`CliTools`]) adds main's
//! agent tools, NOT mod-wrapped, and defers the MCP schemas behind ToolSearch
//! (KERN_DEFERRED_TOOLS) so a big server does not ride on every call.

use std::sync::{Arc, Mutex};

use kern::{
    code::{CodeTool, code_tool_names},
    deferred_tools::{DeferMode, ToolDeferral},
    fs::Fs,
    mcp::McpHub,
    mods::{ModTools, Mods},
    skills::Skill,
    task_board::TaskBoard,
    tool::{Tool, ToolSource},
    tools::{
        BashOptions, BuiltinOptions, ImageTool, ImageToolOptions, JobHub,
        agents::{AgentRef, SubagentHub},
        builtin_tools,
        skill::create_skill_tool,
        tasks::create_task_tools,
        tool_search::deferral_tools,
    },
};

use crate::config::ImageConfig;

/// Everything but MCP: built-ins (BashOutput / KillShell with a job hub), task
/// tools, Skill (over the host's disk: a user skill lives outside any sandbox
/// mount) and GenerateImage.
pub fn own_tools(
    board: Arc<TaskBoard>,
    skills: Arc<Mutex<Vec<Skill>>>,
    host_fs: Arc<dyn Fs>,
    jobs: Option<Arc<JobHub>>,
    image: Option<&ImageConfig>,
) -> Vec<Arc<dyn Tool>> {
    let mut tools = builtin_tools(BuiltinOptions {
        bash: BashOptions {
            jobs,
            ..Default::default()
        },
        ..Default::default()
    });
    tools.extend(create_task_tools(board));
    tools.push(create_skill_tool(
        move || skills.lock().unwrap().clone(),
        host_fs,
    ));
    if let Some(cfg) = image {
        let opts = ImageToolOptions {
            base_url: cfg.base_url.clone(),
            ..ImageToolOptions::new(cfg.api_key.clone())
        };
        let opts = opts.with_models(cfg.model.clone(), cfg.models.clone());
        tools.push(Arc::new(ImageTool::new(opts)));
    }
    tools
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
    /// The extra names Code may call (KERN_CODE_TOOLS); None = no Code tool.
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

/// `all` as one call advertises it: MCP schemas behind ToolSearch, per the deferral's mode.
fn advertise(deferral: &Arc<ToolDeferral>, all: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    deferral.advertise(all, deferral_tools(deferral.clone()))
}

/// A subagent's fixed list behind its own deferral (ToolSearch loads per agent).
pub struct Deferred {
    tools: Vec<Arc<dyn Tool>>,
    deferral: Arc<ToolDeferral>,
}

impl Deferred {
    pub fn new(tools: Vec<Arc<dyn Tool>>, mode: DeferMode) -> Self {
        Self {
            tools,
            deferral: Arc::new(ToolDeferral::new(mode)),
        }
    }
}

impl ToolSource for Deferred {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        advertise(&self.deferral, &self.tools)
    }
}

/// Main's list (TS `tools()`): the wrapped tools and main's agent tools, deferred.
pub struct CliTools {
    wrapped: Arc<WrappedTools>,
    hub: Arc<SubagentHub>,
    deferral: Arc<ToolDeferral>,
}

impl CliTools {
    pub fn new(wrapped: Arc<WrappedTools>, hub: Arc<SubagentHub>, mode: DeferMode) -> Self {
        Self {
            wrapped,
            hub,
            deferral: Arc::new(ToolDeferral::new(mode)),
        }
    }
}

impl ToolSource for CliTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut all = self.wrapped.tools();
        all.extend(self.hub.tools_for(&AgentRef::main()));
        advertise(&self.deferral, &all)
    }
}
