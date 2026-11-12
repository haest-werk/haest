//! A second host, from outside: kern through its public API only, the way any
//! crate that depends on kern-lib sees it. brain (examples/brain) brings its
//! own transport and its own sub-agent runner; this host takes kern's own
//! pieces and plugs its own in beside them:
//!
//! | Plug point | Test |
//! |---|---|
//! | `Provider`: the host's own model backend | every test |
//! | `ShellCommand` in the sandbox shell: pipes, `&&`, stdin, cwd, the host's Fs | `a_host_command_runs_in_the_sandbox_shell_like_a_builtin` |
//! | `Tool` + `ToolOutput::with_title` | `a_host_tool_titles_its_call_for_the_host_and_not_for_the_model` |
//! | `AgentControl::steer` from the host, mid-turn | `the_host_steers_a_running_turn_at_the_next_step` |
//! | `SubagentHub` over the host's `SubagentHost` | `kerns_own_hub_runs_a_sub_agent_with_the_hosts_tool_and_command` |
//! | `Inbox`: a background sub-agent's answer | `a_background_sub_agent_answers_through_the_hosts_inbox` |
//! | `AgentHooks::before_model_call` refusing | `a_hook_may_refuse_a_model_call_and_end_the_turn` |
//! | `SkillSet`: the host's own skills, kern's Skill tool and prompt list | `the_hosts_own_skills_load_through_kerns_skill_tool` |
//! | `DeferredTools`: the host's own tools behind ToolSearch, grow | `a_host_defers_its_own_tools_and_a_loaded_one_joins_the_list` |
//! | the same, proxy (CallTool) | `in_proxy_mode_the_list_never_changes_and_call_tool_runs_a_deferred_one` |

use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use kern::{
    Agent, AgentControl, AgentDeps, AgentHooks, AgentOptions, AssistantMessage, AssistantPart,
    Effort, ExecResult, Fs, LoopEvent, MemFs, Message, Provider, ProviderEvent, ProviderRequest,
    ResolvedModel, StopReason, ToolCall, TurnEnd, Usage, Workspace,
    deferred_tools::{DeferMode, ToolDeferral},
    provider::ProviderStream,
    shell_command::{ShellCall, ShellCommand},
    skills::{InlineSkill, InlineSkills, SkillCard, SkillSet, skill_cards_block},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolHost, ToolOutput, req_str},
    tools::{
        BuiltinOptions, Inbox, SkillToolOptions,
        agents::{AgentRef, SubagentCaps, SubagentEvent, SubagentHost, SubagentHub},
        builtin_tools_where, create_skill_tool,
        tool_search::DeferredTools,
    },
    workspace::SandboxWorkspace,
};
use serde_json::{Value, json};
use tokio::sync::{
    Semaphore,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

const MODEL: &str = "own:assistant-1";
const CWD: &str = "/work";
const PLAN: &str = "ship the q3 review\n";
const SUBAGENT_MARK: &str = "# You are a subagent";

// ---------------------------------------------------------------------------
// The host's model backend: a Provider of its own, no HTTP.

/// One request as the model saw it.
#[derive(Clone)]
struct Seen {
    sub: bool,
    system: String,
    tools: Vec<String>,
    messages: Vec<Message>,
}

/// Main and sub-agents answer from separate scripts (a sub-agent is told apart
/// by kern's sub-agent note in its system prompt). A sub-agent's call waits for
/// a permit from `sub_gate`, so a test decides when a background one finishes.
struct Scripted {
    main: Mutex<VecDeque<Vec<AssistantPart>>>,
    sub: Mutex<VecDeque<Vec<AssistantPart>>>,
    sub_gate: Arc<Semaphore>,
    seen: Mutex<Vec<Seen>>,
}

impl Scripted {
    fn requests(&self, sub: bool) -> Vec<Seen> {
        let seen = self.seen.lock().unwrap();
        seen.iter().filter(|s| s.sub == sub).cloned().collect()
    }
}

impl Provider for Scripted {
    fn id(&self) -> &str {
        "own"
    }
    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let sub = req.system.contains(SUBAGENT_MARK);
        self.seen.lock().unwrap().push(Seen {
            sub,
            system: req.system.clone(),
            tools: req.tools.iter().map(|t| t.name.clone()).collect(),
            messages: req.messages.clone(),
        });
        let script = if sub { &self.sub } else { &self.main };
        let parts = script.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![AssistantPart::Text {
                text: "(the script ran out)".into(),
            }]
        });
        let (gate, model) = (self.sub_gate.clone(), req.model.clone());
        Box::pin(futures::stream::once(async move {
            if sub {
                gate.acquire().await.expect("the gate stays open").forget();
            }
            let mut m = AssistantMessage::start("own", &model);
            let calls = parts
                .iter()
                .any(|p| matches!(p, AssistantPart::ToolCall(_)));
            m.stop_reason = if calls {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            };
            m.content = parts;
            m.usage = Usage {
                cost_usd: Some(0.001),
                ..Default::default()
            };
            ProviderEvent::Done(m)
        }))
    }
}

fn say(text: &str) -> Vec<AssistantPart> {
    vec![AssistantPart::Text { text: text.into() }]
}

fn call(id: &str, name: &str, input: Value) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
    })
}

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
// The host: kern's own sub-agent hub runs over it.

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
impl SubagentHost for App {
    fn deps(&self) -> AgentDeps {
        let provider = self.provider.clone();
        let models = move |spec: &str| {
            Ok::<_, String>(ResolvedModel {
                provider: provider.clone(),
                model: spec.to_string(),
            })
        };
        AgentDeps::new(
            Arc::new(models),
            Arc::new(self.base_tools()),
            Arc::new(self.prompt()),
            CWD,
            self.workspace.clone(),
        )
    }
    fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        let on = |w: &str| matches!(w, "read" | "write" | "bash");
        let mut tools = builtin_tools_where(
            on,
            BuiltinOptions {
                web_fetch: None,
                ..Default::default()
            },
        )
        .expect("read, write and bash need no options");
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
        ..app.deps()
    };
    Rig {
        agent: Agent::new(deps, AgentOptions::new(MODEL)),
        app,
        events: Vec::new(),
        inbox,
    }
}

impl Rig {
    async fn turn(&mut self, text: &str) -> TurnEnd {
        self.agent.run(text, &mut self.events).await
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

fn exit_code(out: &ToolOutput) -> i64 {
    out.result.as_ref().unwrap()["exitCode"].as_i64().unwrap()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_host_command_runs_in_the_sandbox_shell_like_a_builtin() {
    let mut r = rig(
        vec![
            vec![
                // Piped into, piped out of, chained with a builtin.
                call(
                    "c1",
                    "Bash",
                    json!({"command": "printf 'q3,up\\nq2,flat\\n' | stamp notes/out.csv | tr a-z A-Z && cat notes/out.csv"}),
                ),
                // Relative to the shell's cwd after a cd.
                call(
                    "c2",
                    "Bash",
                    json!({"command": "cd notes && printf 'x' | stamp here.txt"}),
                ),
                // Its own failure, as a program's.
                call("c3", "Bash", json!({"command": "stamp"})),
            ],
            say("Stamped."),
        ],
        vec![],
        0,
    );
    assert_eq!(
        r.turn("Stamp the figures.").await,
        TurnEnd::new(StopReason::EndTurn)
    );

    let c1 = r.output("c1");
    assert!(!c1.is_error, "{c1:?}");
    assert!(
        c1.content
            .contains("STAMPED 14 BYTES TO /WORK/NOTES/OUT.CSV"),
        "{}",
        c1.content
    );
    assert!(c1.content.contains("Q3,UP\nQ2,FLAT"), "{}", c1.content);
    assert_eq!(r.file("/work/notes/out.csv").await, "Q3,UP\nQ2,FLAT\n");

    assert!(!r.output("c2").is_error, "{:?}", r.output("c2"));
    assert_eq!(r.file("/work/notes/here.txt").await, "X");

    let c3 = r.output("c3");
    assert!(c3.is_error && exit_code(c3) == 2, "{c3:?}");
    assert!(c3.content.contains("usage: stamp <file>"), "{}", c3.content);

    // Its note reached the model with the workspace's.
    let first = &r.app.provider.requests(false)[0];
    assert!(
        first.system.contains("`stamp <file>` writes its stdin"),
        "{}",
        first.system
    );
}

#[tokio::test]
async fn a_host_tool_titles_its_call_for_the_host_and_not_for_the_model() {
    let mut r = rig(
        vec![
            vec![call("c1", "lookup", json!({"key": "q3"}))],
            say("Q3 is up 12%."),
        ],
        vec![],
        0,
    );
    assert_eq!(
        r.turn("How was Q3?").await,
        TurnEnd::new(StopReason::EndTurn)
    );
    let c1 = r.output("c1");
    assert_eq!(
        (c1.content.as_str(), c1.title.as_deref()),
        ("up 12%", Some("Looked up q3"))
    );

    let second = &r.app.provider.requests(false)[1];
    let Some(Message::Tool(result)) = second.messages.last() else {
        panic!("the second call ends on the tool result");
    };
    assert_eq!(
        result.content, "up 12%",
        "the title is the host's, never the model's"
    );
}

#[tokio::test]
async fn the_host_steers_a_running_turn_at_the_next_step() {
    let mut r = rig(
        vec![
            vec![call("c1", "lookup", json!({"key": "q3"}))],
            vec![call("c2", "lookup", json!({"key": "q2"}))],
            say("Q3 up 12%, Q2 flat."),
        ],
        vec![],
        0,
    );
    // The person types while lookup runs.
    *r.app.lookup.steer.lock().unwrap() = Some((r.agent.control(), "Also check Q2.".into()));
    assert_eq!(
        r.turn("How was Q3?").await,
        TurnEnd::new(StopReason::EndTurn)
    );

    let second = &r.app.provider.requests(false)[1];
    let tail: Vec<&Message> = second.messages.iter().rev().take(2).collect();
    assert!(
        matches!(tail[0], Message::User(u) if u.content.contains("Also check Q2.")),
        "the steer lands after the tool result: {tail:?}"
    );
    assert!(matches!(tail[1], Message::Tool(t) if t.content == "up 12%"));
    assert_eq!(r.app.provider.requests(false).len(), 3);
    assert!(
        r.agent.control().take_queued().is_none(),
        "nothing left over"
    );
}

#[tokio::test]
async fn kerns_own_hub_runs_a_sub_agent_with_the_hosts_tool_and_command() {
    let mut r = rig(
        vec![
            vec![call(
                "c1",
                "Agent",
                json!({"description": "Stamp the plan", "prompt": "Stamp notes/plan.md into notes/PLAN.md, then look up q3."}),
            )],
            say("The helper stamped the plan."),
        ],
        vec![
            vec![
                call(
                    "s1",
                    "Bash",
                    json!({"command": "cat notes/plan.md | stamp notes/PLAN.md"}),
                ),
                call("s2", "lookup", json!({"key": "q3"})),
            ],
            say("Stamped notes/PLAN.md; Q3 is up 12%."),
        ],
        10,
    );
    assert_eq!(
        r.turn("Stamp the plan.").await,
        TurnEnd::new(StopReason::EndTurn)
    );

    // Its answer is main's tool result, its cost the tool's.
    let c1 = r.output("c1");
    assert!(!c1.is_error, "{c1:?}");
    assert!(
        c1.content.contains("Stamped notes/PLAN.md; Q3 is up 12%."),
        "{}",
        c1.content
    );
    assert!(c1.child_cost_usd.is_some_and(|c| c > 0.0), "{c1:?}");

    // It ran the host's command on the host's Fs, and the host's tool.
    assert_eq!(r.file("/work/notes/PLAN.md").await, PLAN.to_uppercase());
    let subs = r.app.provider.requests(true);
    assert_eq!(subs.len(), 2);
    let Some(Message::Tool(s2)) = subs[1].messages.last() else {
        panic!("the sub-agent's second call ends on a tool result");
    };
    assert_eq!(s2.content, "up 12%");

    // The host's prompt and tools, kern's note and agent tools on top.
    assert!(
        subs[0].system.contains("You keep the books"),
        "{}",
        subs[0].system
    );
    assert!(subs[0].system.contains("`stamp <file>`"));
    for name in ["Read", "Write", "Bash", "lookup", "SendMessage"] {
        assert!(
            subs[0].tools.iter().any(|t| t == name),
            "{name} in {:?}",
            subs[0].tools
        );
    }
    let events = r.app.sub_events.lock().unwrap().clone();
    assert_eq!(events.first(), Some(&"started"));
    assert_eq!(events.last(), Some(&"stopped"));
    assert!(events.contains(&"loop"));
}

#[tokio::test]
async fn a_background_sub_agent_answers_through_the_hosts_inbox() {
    let mut r = rig(
        vec![
            vec![call(
                "c1",
                "Agent",
                json!({"description": "Count the notes", "prompt": "How many notes are there?", "run_in_background": true}),
            )],
            say("A helper is counting."),
        ],
        vec![say("One note: notes/plan.md.")],
        // The helper may not answer until main's turn has ended.
        0,
    );
    assert_eq!(
        r.turn("Count the notes.").await,
        TurnEnd::new(StopReason::EndTurn)
    );
    let c1 = r.output("c1");
    assert!(!c1.is_error, "{c1:?}");
    assert!(
        r.inbox.try_recv().is_err(),
        "nothing yet: the helper still works"
    );

    r.app.provider.sub_gate.add_permits(1);
    let answer = tokio::time::timeout(Duration::from_secs(5), r.inbox.recv())
        .await
        .expect("the answer arrives")
        .expect("the inbox is open");
    assert!(answer.contains("One note: notes/plan.md."), "{answer}");

    // The host runs it as main's next turn.
    r.app
        .provider
        .main
        .lock()
        .unwrap()
        .push_back(say("There is one note."));
    assert_eq!(r.turn(&answer).await, TurnEnd::new(StopReason::EndTurn));
}

/// A host that ends the turn before a model call: its budget is spent.
struct Budget {
    calls: AtomicUsize,
    allowed: usize,
}

impl ToolHost for Budget {}

#[async_trait]
impl AgentHooks for Budget {
    async fn before_model_call(
        &self,
        _agent: &mut Agent,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.allowed {
            return Ok(());
        }
        Err("The month's budget is spent.".into())
    }
}

#[tokio::test]
async fn a_hook_may_refuse_a_model_call_and_end_the_turn() {
    let mut r = rig(
        vec![
            vec![call("c1", "lookup", json!({"key": "q3"}))],
            say("never sent"),
        ],
        vec![],
        0,
    );
    let mut deps = r.app.deps();
    deps.tools = r.agent.deps().tools.clone();
    deps.hooks = Arc::new(Budget {
        calls: AtomicUsize::new(0),
        allowed: 1,
    });
    r.agent = Agent::new(deps, AgentOptions::new(MODEL));

    let end = r.turn("How was Q3?").await;
    assert_eq!(
        end,
        TurnEnd::with_error(StopReason::Error, "The month's budget is spent.")
    );
    assert_eq!(
        r.app.provider.requests(false).len(),
        1,
        "no call after the refusal"
    );
    // The history stays whole: the call it made has its result.
    let kinds: Vec<&str> = r
        .agent
        .messages
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::Tool(_) => "tool",
        })
        .collect();
    assert_eq!(kinds, ["user", "assistant", "tool"]);
}

// ---------------------------------------------------------------------------
// The host's own skills (not SKILL.md files on disk) through kern's Skill tool
// and prompt list; the host's own tools deferred behind ToolSearch.

fn skill(name: &str, origin: &str, description: &str, when: &str, body: &str) -> InlineSkill {
    InlineSkill {
        card: SkillCard {
            name: name.into(),
            description: description.into(),
            when: when.into(),
            origin: Some(origin.into()),
        },
        body: body.into(),
    }
}

#[tokio::test]
async fn the_hosts_own_skills_load_through_kerns_skill_tool() {
    let mut r = rig(
        vec![
            vec![call("c1", "Skill", json!({"skill": "standup"}))],
            say("Did it your way."),
        ],
        vec![],
        0,
    );
    // The person's own standup wins over the built-in one of that name.
    let set = Arc::new(InlineSkills::merged([
        vec![skill(
            "standup",
            "yours",
            "How you run a standup",
            "a standup",
            "1. What I did.",
        )],
        vec![
            skill("standup", "built in", "A standup", "", "THE DEFAULT"),
            skill("board", "built in", "Keep a board", "", "Columns."),
        ],
    ]));
    let intro = "Skills: load one with Skill before you start.";
    let mut deps = r.app.deps();
    let mut tools = r.app.base_tools();
    tools.push(create_skill_tool(
        SkillToolOptions::over(set.clone()).with_name_enum(),
    ));
    deps.tools = Arc::new(tools);
    deps.system_prompt = Arc::new(format!(
        "{}\n\n{}",
        r.app.prompt(),
        skill_cards_block(intro, &set.offered())
    ));
    r.agent = Agent::new(deps, AgentOptions::new(MODEL));

    assert_eq!(
        r.turn("Standup time.").await,
        TurnEnd::new(StopReason::EndTurn)
    );
    let first = &r.app.provider.requests(false)[0];
    assert!(
        first.system.contains(
            "Skills: load one with Skill before you start.\n- board (built in): Keep a board\n- standup (yours): How you run a standup Use when: a standup"
        ),
        "{}",
        first.system
    );
    assert!(
        !first.system.contains("1. What I did."),
        "bodies never ride in the prompt"
    );
    assert_eq!(
        r.output("c1").content,
        "Skill standup (yours): How you run a standup\n\n1. What I did."
    );
}

/// The rig's agent over `source` instead of its plain list.
fn deferring(r: &mut Rig, source: DeferredTools) {
    let mut deps = r.app.deps();
    deps.tools = Arc::new(source);
    r.agent = Agent::new(deps, AgentOptions::new(MODEL));
}

#[tokio::test]
async fn a_host_defers_its_own_tools_and_a_loaded_one_joins_the_list() {
    let mut r = rig(
        vec![
            vec![call("c1", "ToolSearch", json!({"query": "figure records"}))],
            vec![call("c2", "lookup", json!({"key": "q3"}))],
            // Never searched for: called by name, it runs anyway.
            vec![call(
                "c3",
                "Write",
                json!({"file_path": "/work/q3.md", "content": "up"}),
            )],
            say("Q3 is up 12%, written down."),
        ],
        vec![],
        0,
    );
    let tools = r.app.base_tools();
    deferring(
        &mut r,
        DeferredTools::new(
            tools,
            ToolDeferral::new(DeferMode::Grow).deferring_all_except(["Bash".to_string()]),
        ),
    );
    assert_eq!(
        r.turn("How was Q3?").await,
        TurnEnd::new(StopReason::EndTurn)
    );

    let seen = r.app.provider.requests(false);
    assert_eq!(
        seen[0].tools,
        ["Bash", "ToolSearch"],
        "only Bash and the search go out"
    );
    let found = &r.output("c1").content;
    assert!(found.contains("\"name\":\"lookup\""), "{found}");
    assert_eq!(
        seen[1].tools,
        ["Bash", "ToolSearch", "lookup"],
        "a loaded tool joins"
    );
    assert_eq!(r.output("c2").content, "up 12%");
    assert!(!r.output("c3").is_error, "{:?}", r.output("c3"));
    assert_eq!(r.file("/work/q3.md").await, "up");
    // Loaded tools keep the host's order: the list depends on what is loaded,
    // never on the order it loaded in.
    assert_eq!(
        seen[3].tools,
        ["Bash", "ToolSearch", "Write", "lookup"],
        "a tool called by name counts as loaded"
    );
}

#[tokio::test]
async fn in_proxy_mode_the_list_never_changes_and_call_tool_runs_a_deferred_one() {
    let mut r = rig(
        vec![
            vec![call("c1", "ToolSearch", json!({"query": "select:lookup"}))],
            vec![call(
                "c2",
                "CallTool",
                json!({"name": "lookup", "input": {"key": "q2"}}),
            )],
            say("Q2 was flat."),
        ],
        vec![],
        0,
    );
    let tools = r.app.base_tools();
    deferring(
        &mut r,
        DeferredTools::new(
            tools,
            ToolDeferral::new(DeferMode::Proxy).deferring_all_except(["Bash".to_string()]),
        ),
    );
    assert_eq!(r.turn("And Q2?").await, TurnEnd::new(StopReason::EndTurn));
    let lists: Vec<Vec<String>> = r
        .app
        .provider
        .requests(false)
        .into_iter()
        .map(|s| s.tools)
        .collect();
    assert!(
        lists
            .iter()
            .all(|l| l == &["Bash", "ToolSearch", "CallTool"]),
        "the cached prefix holds: {lists:?}"
    );
    let c2 = r.output("c2");
    assert_eq!(
        (c2.content.as_str(), c2.title.as_deref()),
        ("flat", Some("Looked up q2"))
    );
}
