//! A second host, from outside: haest through its public API only, the way any
//! crate that depends on haest sees it. brain (examples/brain) brings its
//! own HTTP client and its own sub-agent runner; this host takes haest's own
//! pieces and plugs its own in beside them. The host is here; the tests, by
//! plug point, are in `plug_in/`:
//!
//! | Plug point | Test |
//! |---|---|
//! | `Provider`: the host's own model backend | every test |
//! | `ShellCommand` in the sandbox shell: pipes, `&&`, stdin, cwd, the host's Fs | `turns::a_host_command_runs_in_the_sandbox_shell_like_a_builtin` |
//! | `Tool` + `ToolOutput::with_title` | `turns::a_host_tool_titles_its_call_for_the_host_and_not_for_the_model` |
//! | `AgentControl::steer` from the host, mid-turn | `turns::the_host_steers_a_running_turn_at_the_next_step` |
//! | `AgentHooks::before_model_call` refusing | `turns::a_hook_may_refuse_a_model_call_and_end_the_turn` |
//! | `SubagentHub` over the host's `SubagentParent` | `subagents::haests_own_hub_runs_a_sub_agent_with_the_hosts_tool_and_command` |
//! | `Inbox`: a background sub-agent's answer | `subagents::a_background_sub_agent_answers_through_the_hosts_inbox` |
//! | `SkillSet`: the host's own skills, haest's Skill tool and prompt list | `skills::the_hosts_own_skills_load_through_haests_skill_tool` |
//! | `DeferredTools`: the host's own tools behind ToolSearch, grow | `skills::a_host_defers_its_own_tools_and_a_loaded_one_joins_the_list` |
//! | the same, proxy (CallTool) | `skills::in_proxy_mode_the_list_never_changes_and_call_tool_runs_a_deferred_one` |

// A crate root's modules sit beside it; these sit in plug_in/, where cargo
// does not take each for a test target of its own.
#[path = "plug_in/model.rs"]
mod model;
#[path = "plug_in/skills.rs"]
mod skills;
#[path = "plug_in/subagents.rs"]
mod subagents;
#[path = "plug_in/turns.rs"]
mod turns;

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest::{
    Agent, AgentControl, AgentDeps, AgentOptions, AssistantMessage, AssistantPart, Effort,
    ExecResult, Fs, LoopEvent, MemFs, Message, ModelError, Provider, ProviderEvent,
    ProviderRequest, ResolvedModel, SharedDeps, StopReason, ToolCall, TurnOutcome, Usage,
    Workspace,
    core::caps::SubagentCaps,
    core::provider::ProviderStream,
    core::shell_command::{ShellCall, ShellCommand},
    core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str},
    tools::{
        AgentRef, BuiltinOptions, Inbox, SubagentEvent, SubagentHub, SubagentParent,
        builtin_tools_where,
    },
    workspace::SandboxWorkspace,
};
use serde_json::{Value, json};
use tokio::sync::{
    Semaphore,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

use model::{Scripted, call, say};

const MODEL: &str = "own:assistant-1";
const CWD: &str = "/work";
const PLAN: &str = "ship the q3 review\n";
const SUBAGENT_MARK: &str = "# You are a subagent";

// ---------------------------------------------------------------------------
// The host's shell command: `stamp <file>` writes its stdin, upper-cased, to
// <file> through the host's Fs.

struct Stamp;

#[async_trait]
impl ShellCommand for Stamp {
    fn name(&self) -> &str {
        "stamp"
    }
    fn note(&self) -> Option<String> {
        Some("`stamp <file>` writes its stdin, upper-cased, to <file>.".into())
    }
    async fn run(&self, call: ShellCall) -> ExecResult {
        let Some(out) = call.args.first() else {
            return ExecResult {
                stderr: "usage: stamp <file>\n".into(),
                exit_code: 2,
                ..Default::default()
            };
        };
        let text = call.stdin_text().unwrap_or_default().to_uppercase();
        let path = call.cwd.join(out);
        if let Err(e) = call.fs.write(&path, text.as_bytes()).await {
            return ExecResult {
                stderr: format!("stamp: {e}\n"),
                exit_code: 1,
                ..Default::default()
            };
        }
        ExecResult {
            stdout: format!("stamped {} bytes to {}\n", text.len(), path.display()),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// The host's tool: `lookup` answers from the host's own data and titles the
// call for the host's display. While it runs, the host may steer the turn
// (the person typed something).

#[derive(Default)]
struct Lookup {
    steer: Mutex<Option<(AgentControl, String)>>,
}

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }
    fn kind(&self) -> &str {
        "own.lookup"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "Look a figure up in the host's records.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"key": {"type": "string"}},
            "required": ["key"]
        })
    }
    async fn execute(&self, input: JsonMap, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let key = req_str(&input, "key")?;
        if let Some((control, text)) = self.steer.lock().unwrap().take() {
            control.steer(text);
        }
        let value = match key {
            "q3" => "up 12%",
            "q2" => "flat",
            _ => return Err(ToolError::Input(format!("no figure \"{key}\""))),
        };
        Ok(ToolOutput::text(value).with_title(format!("Looked up {key}")))
    }
}

// ---------------------------------------------------------------------------
// The host: haest's own sub-agent hub runs over it.

struct App {
    provider: Arc<Scripted>,
    fs: Arc<MemFs>,
    workspace: Arc<SandboxWorkspace>,
    lookup: Arc<Lookup>,
    inbox: UnboundedSender<String>,
    /// Every sub-agent event kind, in order.
    sub_events: Mutex<Vec<&'static str>>,
}

impl App {
    fn prompt(&self) -> String {
        format!(
            "You keep the books for a small team.\n\n{}",
            self.workspace.prompt_note()
        )
    }
}

impl Inbox for App {
    fn submit(&self, text: String) {
        let _ = self.inbox.send(text);
    }
}

#[async_trait]
impl SubagentParent for App {
    fn deps(&self) -> SharedDeps {
        let provider = self.provider.clone();
        let models = move |spec: &str| {
            Ok::<_, ModelError>(ResolvedModel {
                provider: provider.clone(),
                model: spec.to_string(),
            })
        };
        SharedDeps::new(Arc::new(models), CWD, self.workspace.clone())
    }
    fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        let on = |w: &str| matches!(w, "read" | "write" | "bash");
        let opts = BuiltinOptions {
            web_fetch: None,
            ..Default::default()
        };
        let mut tools =
            builtin_tools_where(on, opts).expect("read, write and bash need no options");
        tools.push(self.lookup.clone());
        tools
    }
    async fn system_prompt(&self, _model_spec: &str, _tools: &[Arc<dyn Tool>]) -> String {
        self.prompt()
    }
    fn default_model(&self) -> (String, Option<Effort>) {
        (MODEL.into(), None)
    }
    fn on_subagent(&self, _id: &str, event: SubagentEvent) {
        let kind = match event {
            SubagentEvent::Started { .. } => "started",
            SubagentEvent::Loop(_) => "loop",
            SubagentEvent::InnerCall(_) => "inner",
            SubagentEvent::Stopped { .. } => "stopped",
        };
        self.sub_events.lock().unwrap().push(kind);
    }
}

struct Rig {
    app: Arc<App>,
    agent: Agent,
    events: Vec<LoopEvent>,
    inbox: UnboundedReceiver<String>,
}

/// A conversation: main answers `main`, sub-agents `sub` (each after a permit
/// from the gate, `sub_permits` of them up front).
fn rig(main: Vec<Vec<AssistantPart>>, sub: Vec<Vec<AssistantPart>>, sub_permits: usize) -> Rig {
    let provider = Arc::new(Scripted {
        main: Mutex::new(main.into()),
        sub: Mutex::new(sub.into()),
        sub_gate: Arc::new(Semaphore::new(sub_permits)),
        seen: Mutex::default(),
    });
    let fs = Arc::new(MemFs::with_files([("/work/notes/plan.md", PLAN)]));
    let workspace = Arc::new(SandboxWorkspace::over(fs.clone()).with_command(Arc::new(Stamp)));
    let (tx, inbox) = unbounded_channel();
    let app = Arc::new(App {
        provider,
        fs,
        workspace,
        lookup: Arc::default(),
        inbox: tx,
        sub_events: Mutex::default(),
    });
    // Main's agent tools hold the hub: it lives as long as the conversation.
    let hub = SubagentHub::new(app.clone(), SubagentCaps::default());
    let mut tools = app.base_tools();
    tools.extend(hub.tools_for(&AgentRef::main()));
    let deps = AgentDeps {
        tools: Arc::new(tools),
        system_prompt: Arc::new(app.prompt()),
        shared: app.deps(),
    };
    Rig {
        agent: Agent::new(deps, AgentOptions::new(MODEL)),
        app,
        events: Vec::new(),
        inbox,
    }
}

/// A conversation where only main answers.
fn main_rig(main: Vec<Vec<AssistantPart>>) -> Rig {
    rig(main, vec![], 0)
}

impl Rig {
    async fn turn(&mut self, text: &str) -> TurnOutcome {
        self.agent.run(text, &mut self.events).await
    }

    /// A turn that ends on the model's answer.
    async fn answered(&mut self, text: &str) {
        assert_eq!(self.turn(text).await, TurnOutcome::new(StopReason::EndTurn));
    }

    /// The agent again, over the host's deps as `edit` changes them: the
    /// base tools, the host's prompt, what every agent shares.
    fn reseat(&mut self, edit: impl FnOnce(&mut AgentDeps)) {
        let mut deps = AgentDeps {
            tools: Arc::new(self.app.base_tools()),
            system_prompt: Arc::new(self.app.prompt()),
            shared: self.app.deps(),
        };
        edit(&mut deps);
        self.agent = Agent::new(deps, AgentOptions::new(MODEL));
    }

    fn output(&self, id: &str) -> &ToolOutput {
        self.events
            .iter()
            .find_map(|ev| match ev {
                LoopEvent::ToolResult { call, output, .. } if call.id == id => Some(output),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no result for tool call {id}"))
    }

    async fn file(&self, path: &str) -> String {
        let bytes = self.app.fs.read(Path::new(path)).await.expect(path);
        String::from_utf8(bytes).unwrap()
    }
}
