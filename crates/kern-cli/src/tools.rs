//! The tools a CLI conversation offers, composed as the TS kern does
//! (`host/runtime.ts`): the built-ins, the task list, Skill and every connected
//! MCP server's tools; the Code tool over them; all of it inside the mods'
//! `tool.before` / `tool.after` hooks; and the MCP schemas deferred behind
//! ToolSearch (KERN_DEFERRED_TOOLS) so a big server does not ride on every call.

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
        BuiltinOptions, builtin_tools, skill::create_skill_tool, tasks::create_task_tools,
        tool_search::deferral_tools,
    },
};

/// Everything but MCP: built-ins, task tools, Skill (over the host's disk:
/// a user skill lives outside any sandbox mount).
pub fn own_tools(
    board: Arc<TaskBoard>,
    skills: Arc<Mutex<Vec<Skill>>>,
    host_fs: Arc<dyn Fs>,
) -> Vec<Arc<dyn Tool>> {
    let mut tools = builtin_tools(BuiltinOptions::default());
    tools.extend(create_task_tools(board));
    tools.push(create_skill_tool(
        move || skills.lock().unwrap().clone(),
        host_fs,
    ));
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

pub struct CliTools {
    base: Arc<BaseTools>,
    mods: Arc<Mods>,
    /// The extra names Code may call (KERN_CODE_TOOLS); None = no Code tool.
    code: Option<Vec<String>>,
    deferral: Arc<ToolDeferral>,
}

impl CliTools {
    pub fn new(
        own: Vec<Arc<dyn Tool>>,
        mcp: Arc<McpHub>,
        mods: Arc<Mods>,
        code: Option<Vec<String>>,
        mode: DeferMode,
    ) -> Self {
        Self {
            base: Arc::new(BaseTools { own, mcp }),
            mods,
            code,
            deferral: Arc::new(ToolDeferral::new(mode)),
        }
    }

    /// Every tool, deferral aside, inside the mods' hooks. Code calls the
    /// WRAPPED siblings, read per run, so every call it makes passes the hooks too.
    fn wrapped(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base.tools();
        if let Some(extra) = &self.code {
            let names = code_tool_names(&tools, extra);
            let callable = Arc::new(ModTools::new(self.base.clone(), self.mods.clone()));
            tools.push(Arc::new(CodeTool::new(names, callable)));
        }
        ModTools::wrap(&self.mods, tools)
    }
}

impl ToolSource for CliTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.deferral
            .advertise(&self.wrapped(), deferral_tools(self.deferral.clone()))
    }
}
