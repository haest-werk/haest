//! The tools a CLI conversation offers: the built-ins, the task list, Skill,
//! and every connected MCP server's tools -- the MCP ones deferred behind
//! ToolSearch (KERN_DEFERRED_TOOLS) so a big server does not ride on every call.

use std::sync::{Arc, Mutex};

use kern::{
    deferred_tools::{DeferMode, ToolDeferral},
    fs::Fs,
    mcp::McpHub,
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

pub struct CliTools {
    own: Vec<Arc<dyn Tool>>,
    mcp: Arc<McpHub>,
    deferral: Arc<ToolDeferral>,
}

impl CliTools {
    pub fn new(own: Vec<Arc<dyn Tool>>, mcp: Arc<McpHub>, mode: DeferMode) -> Self {
        Self {
            own,
            mcp,
            deferral: Arc::new(ToolDeferral::new(mode)),
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.tools().iter().map(|t| t.name().to_string()).collect()
    }
}

impl ToolSource for CliTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut all = self.own.clone();
        all.extend(self.mcp.tools());
        self.deferral
            .advertise(&all, deferral_tools(self.deferral.clone()))
    }
}
