//! One conversation built from what the host hands in: providers, workspace,
//! tools (MCP servers included), mods, subagents, the inbox and the session
//! file -- resumed or new -- and the agent over all of it.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, anyhow, bail};
use kern_settings::Settings;

use crate::{
    Agents, Host, Ui,
    inbox::{self, InboxQueue},
    kern::{
        Agent, AgentDeps, AgentOptions, KeySource,
        fs::Fs,
        ledger::Ledger,
        mcp::{McpHub, McpServerConfig},
        mods::{ModSystemPrompt, Mods, Prompter},
        providers::{CustomProviders, ProviderRegistry, RegistryOptions, RoutingEnv},
        session::{JsonlSessionFile, SessionRecord},
        skills::{Skill, SkillOpts, discover_skills},
        task_board::TaskBoard,
        tool::Tool,
        tools::{JobHub, agents::SubagentHub},
    },
    mods, now_ms,
    prompt::{AgentPrompt, RuntimePrompt, skill_opts},
    sessions::{latest_in, new_session_id, sessions_dir},
    tools::{MainTools, WrappedTools, image_options, own_tools},
    world::{World, connect_mcp, jobs_for, workspace_for},
};

/// Everything a conversation is built from. kern reads no file, variable or
/// flag of its own: what is not in here does not exist for it.
pub struct RuntimeInputs {
    pub settings: Settings,
    pub world: World,
    /// What the host answers for a provider variable (`OPENROUTER_API_KEY`,
    /// `ANTHROPIC_BASE_URL`, ...): its environment, a vault, a fixed map.
    pub keys: Arc<dyn KeySource>,
    /// providers.json's entries, by name.
    pub providers: CustomProviders,
    /// The MCP servers to start.
    pub mcp_servers: Vec<McpServerConfig>,
    /// OpenRouter's `provider` routing; None = none goes out.
    pub routing: Option<RoutingEnv>,
}

impl RuntimeInputs {
    /// The settings in this world, and nothing else: no keys, no custom
    /// providers, no MCP servers, no routing.
    pub fn new(settings: Settings, world: World) -> Self {
        let no_keys = |_: &str| None;
        Self {
            settings,
            world,
            keys: Arc::new(no_keys),
            providers: CustomProviders::new(),
            mcp_servers: Vec::new(),
            routing: None,
        }
    }
}

pub struct Runtime<U: ?Sized> {
    pub agent: Agent,
    pub host: Arc<Host>,
    /// The host's own UI, as it handed it in.
    pub ui: Arc<U>,
    pub fs: Arc<dyn Fs>,
    pub board: Arc<TaskBoard>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    pub skill_opts: SkillOpts,
    pub mcp: Arc<McpHub>,
    pub mods: Arc<Mods>,
    pub session_id: String,
    /// Turns the human did not start, waiting for main.
    pub inbox: InboxQueue,
    /// Main's subagents (none run when `agents` is off).
    pub agents: Arc<SubagentHub>,
    /// Background shell jobs; None unless Bash runs in the real workspace.
    pub jobs: Option<Arc<JobHub>>,
}

/// What a host decides per run, beside the settings.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// None = a new session; Some("") = the latest one in the cwd; Some(id) = that one.
    pub resume: Option<String>,
}

/// Which session this run writes: a new one, the one named, or the latest here.
async fn session_id_for(
    fs: &dyn Fs,
    s: &Settings,
    resume: Option<&str>,
) -> anyhow::Result<(String, bool)> {
    match resume {
        None => Ok((new_session_id(), false)),
        Some("") => latest_in(fs, &sessions_dir(&s.data_dir), &s.cwd)
            .await
            .map(|id| (id, true))
            .ok_or_else(|| anyhow!("no saved session in {} to resume", s.cwd.display())),
        Some(id) => Ok((id.to_string(), true)),
    }
}

pub fn open_store(fs: &Arc<dyn Fs>, data_dir: &Path, id: &str) -> anyhow::Result<JsonlSessionFile> {
    JsonlSessionFile::with_error_handler(fs.clone(), data_dir, id, |e| {
        eprintln!("kern: session log write failed: {e}")
    })
    .map_err(|e| anyhow!("{e}"))
}

/// A new session file: header, the model and effort it starts on, the ledger so far.
pub fn init_store(host: &Host, agent: &Agent, id: &str) {
    host.store()
        .init(id, &host.settings.cwd.to_string_lossy(), now_ms());
    host.append(SessionRecord::Config {
        t: now_ms(),
        model: Some(agent.model_spec.clone()),
        effort: Some(agent.effort),
    });
    if let Some(ledger) = &agent.deps().ledger {
        let snapshot = ledger.lock().unwrap().snapshot();
        host.append(SessionRecord::Ledger {
            t: now_ms(),
            ledger: snapshot,
        });
    }
}

/// The whole conversation, from what the host hands in; `prompter` answers
/// the approval mod's asks.
pub async fn build<U, P>(
    inputs: RuntimeInputs,
    ui: Arc<U>,
    prompter: P,
    opts: BuildOptions,
) -> anyhow::Result<Runtime<U>>
where
    U: Ui + 'static,
    P: Prompter + 'static,
{
    let RuntimeInputs {
        settings,
        world,
        keys,
        providers,
        mcp_servers,
        routing,
    } = inputs;
    let fs = world.fs();
    let reg_opts = RegistryOptions {
        routing,
        ..Default::default()
    };
    let registry = ProviderRegistry::new(keys.clone(), providers, reg_opts);
    let (session_id, resuming) =
        session_id_for(fs.as_ref(), &settings, opts.resume.as_deref()).await?;
    let store = Arc::new(open_store(&fs, &settings.data_dir, &session_id)?);
    if resuming && !store.exists().await {
        bail!("no saved session \"{session_id}\" (kern sessions lists them)");
    }
    let host = Arc::new(Host::new(settings, ui.clone(), store.clone()));
    let s = &host.settings;

    let skill_opts = skill_opts(s);
    let skills = Arc::new(Mutex::new(discover_skills(fs.as_ref(), &skill_opts).await));
    let spill = s.spill_dir(&session_id);
    let workspace = workspace_for(s, &world, &skill_opts, &spill).await?;
    let board = Arc::new(TaskBoard::new({
        let host = host.clone();
        move |tasks| {
            host.append(SessionRecord::Tasks {
                t: now_ms(),
                tasks: tasks.to_vec(),
            })
        }
    }));
    let mcp = connect_mcp(&host, &world, mcp_servers).await;
    let mods = mods::load(&host, fs.as_ref(), prompter).await;
    let (sender, inbox) = inbox::inbox(&host, s.max_agent_turns);
    let jobs = jobs_for(s, sender.clone());
    let image = image_options(s, keys.as_ref()).unwrap_or_else(|why| {
        host.warn(&why);
        None
    });
    let own = own_tools(
        &s.tools,
        board.clone(),
        skills.clone(),
        fs.clone(),
        jobs.clone(),
        image,
    );
    let wrapped = Arc::new(WrappedTools::new(
        own,
        mcp.clone(),
        mods.clone(),
        s.code_tools(),
    ));
    let prompt = Arc::new(RuntimePrompt::new(&host, &fs, &skills, workspace.as_ref()).await);
    let ledger = Arc::new(Mutex::new(Ledger::new(s.ledger(), host.clone())));
    let none: Vec<Arc<dyn Tool>> = Vec::new();
    let mut deps = AgentDeps::new(
        Arc::new(registry),
        Arc::new(none),
        Arc::new(String::new()),
        s.cwd.clone(),
        workspace,
    );
    deps.spill_dir = Some(spill);
    deps.hooks = host.clone();
    deps.ledger = Some(ledger.clone());
    deps.web_search = s.web_search_config();
    let agents = Agents {
        host: host.clone(),
        deps: deps.clone(),
        tools: wrapped.clone(),
        prompt: prompt.clone(),
        mods: mods.clone(),
        defer: s.deferred_tools,
        inbox: sender,
    }
    .hub(s.subagents);
    let main_hub = s.agents.then(|| agents.clone());
    let tools = Arc::new(MainTools::new(wrapped, main_hub, s.deferred_tools));
    let main_prompt = AgentPrompt {
        prompt,
        model: None,
        tools: tools.clone(),
    };
    deps.tools = tools;
    deps.system_prompt = Arc::new(ModSystemPrompt::new(Arc::new(main_prompt), mods.clone()));

    let mut agent_opts = AgentOptions::new(s.model.clone());
    agent_opts.effort = s.effort;
    if resuming {
        let saved = store.load().await.context("reading the session")?;
        if saved.corrupt_lines > 0 {
            host.warn(&format!(
                "Skipped {} torn line(s) in the session file.",
                saved.corrupt_lines
            ));
        }
        if let Some(l) = &saved.ledger {
            ledger.lock().unwrap().restore(l);
        }
        board.restore(saved.tasks.unwrap_or_default());
        agent_opts.model = saved.model.unwrap_or(agent_opts.model);
        agent_opts.effort = saved.effort.unwrap_or(agent_opts.effort);
        agent_opts.history = saved.messages;
    }
    let agent = Agent::new(deps, agent_opts);
    host.set_model(&agent.model_spec);
    host.set_effort(agent.effort);
    if !resuming {
        init_store(&host, &agent, &session_id);
    }
    Ok(Runtime {
        agent,
        host,
        ui,
        fs,
        board,
        skills,
        skill_opts,
        mcp,
        mods,
        session_id,
        inbox,
        agents,
        jobs,
    })
}
