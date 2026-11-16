//! One conversation built from what the host hands in: providers, workspace,
//! tools (MCP servers included), mods, subagents, the inbox and the session
//! file -- resumed or new -- and the agent over all of it.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use kern_settings::Settings;

use crate::{
    Agents, Host, Runtime, Ui, inbox,
    kern::{
        Agent, AgentDeps, AgentOptions, ChildEnv, KeySource, Workspace,
        fs::Fs,
        ledger::Ledger,
        mcp::McpServerConfig,
        mods::{ModSystemPrompt, Prompter},
        now_ms,
        providers::{CustomProviders, ProviderRegistry, RegistryOptions, RoutingEnv},
        session::SessionRecord,
        skills::{Skill, SkillScanRoots, scan_roots, skill_scan_roots, skills_in},
        task_board::TaskBoard,
        tool::Tool,
    },
    mods,
    prompt::{AgentPrompt, RuntimePrompt, skill_opts},
    store::{create, open_session, record_given, record_start, restore, start_on},
    tools::{MainTools, WrappedTools, image_options, own_tools},
    world::{World, connect_mcp, jobs_for, workspace_for},
};

/// Everything a conversation is built from. kern reads no file, variable or
/// flag of its own: what is not in here does not exist for it.
pub struct RuntimeInputs {
    pub settings: Settings,
    pub world: World,
    /// Variables the host passes on purpose (`--env`): every process kern
    /// starts gets them over the world's `child_env`, the sandbox's shell
    /// included (it gets nothing else of the host's environment).
    pub env: ChildEnv,
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
    /// The settings in this world, and nothing else: no variables passed, no
    /// keys, no custom providers, no MCP servers, no routing.
    pub fn new(settings: Settings, world: World) -> Self {
        let no_keys = |_: &str| None;
        Self {
            settings,
            world,
            env: ChildEnv::default(),
            keys: Arc::new(no_keys),
            providers: CustomProviders::new(),
            mcp_servers: Vec::new(),
            routing: None,
        }
    }
}

/// What a host decides per run, beside the settings.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// None = a new session; Some("") = the latest one in the cwd; Some(id) = that one.
    pub resume: Option<String>,
    /// The settings' model was asked for on purpose for this run (a flag):
    /// a resume goes on with it, not with the one the session saved.
    pub model_given: bool,
    /// The same, for the effort.
    pub effort_given: bool,
}

/// What the conversation finds in its world: the skill roots and the skills
/// in them, the workspace (its sandbox mounts those same roots), the prompt's
/// fixed parts.
struct Found {
    skill_roots: SkillScanRoots,
    skills: Arc<Mutex<Vec<Skill>>>,
    workspace: Arc<dyn Workspace>,
    prompt: Arc<RuntimePrompt>,
}

async fn find(
    host: &Arc<Host>,
    world: &World,
    env: &ChildEnv,
    fs: &Arc<dyn Fs>,
    spill: &Path,
) -> anyhow::Result<Found> {
    let skill_roots = skill_scan_roots(fs.as_ref(), &skill_opts(&host.settings)).await;
    let skills = Arc::new(Mutex::new(skills_in(fs.as_ref(), &skill_roots).await));
    let skill_dirs = scan_roots(&skill_roots);
    let workspace = workspace_for(&host.settings, world, env, &skill_dirs, spill).await?;
    let prompt = Arc::new(RuntimePrompt::new(host, fs, &skills, workspace.as_ref()).await);
    Ok(Found {
        skill_roots,
        skills,
        workspace,
        prompt,
    })
}

/// The task list, saved to the session file on every change.
fn task_board(host: &Arc<Host>) -> Arc<TaskBoard> {
    let host = host.clone();
    Arc::new(TaskBoard::new(move |tasks| {
        host.append(SessionRecord::Tasks {
            t: now_ms(),
            tasks: tasks.to_vec(),
        })
    }))
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
        env,
        keys,
        providers,
        mcp_servers,
        routing,
    } = inputs;
    let fs = world.fs();
    let session = open_session(&fs, &settings, opts.resume.as_deref(), ui.clone()).await?;
    let spill = settings.spill_dir(session.store.session_id());
    let host = Arc::new(Host::new(settings, ui.clone(), session.store.clone()));
    let s = &host.settings;

    // Whatever can fail runs before anything starts, so a failed build has
    // started nothing and left no file: the files are found, then a new
    // session's file is created. Then the MCP servers and the mods start,
    // together; from there on nothing fails.
    let found = find(&host, &world, &env, &fs, &spill).await?;
    if session.saved.is_none() {
        create(&session.store, &s.cwd).await?;
    }
    let (mcp, mods) = tokio::join!(
        connect_mcp(&host, &world, &env, mcp_servers),
        mods::load(&host, fs.as_ref(), prompter),
    );

    let board = task_board(&host);
    let (sender, inbox) = inbox::inbox(&host, s.max_agent_turns);
    let jobs = jobs_for(s, sender.clone());
    let image = image_options(s, keys.as_ref()).unwrap_or_else(|why| {
        host.warn(&why);
        None
    });
    let own = own_tools(
        &s.tools,
        board.clone(),
        found.skills.clone(),
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
    let ledger = Arc::new(Mutex::new(Ledger::new(s.ledger(), host.clone())));
    let reg_opts = RegistryOptions {
        routing,
        ..Default::default()
    };
    let registry = ProviderRegistry::new(keys, providers, reg_opts);

    // The conversation's deps. Tools and prompt are each agent's own: the hub
    // gives every subagent its own, and main's tools hold the hub.
    let none: Vec<Arc<dyn Tool>> = Vec::new();
    let empty = AgentDeps::new(
        Arc::new(registry),
        Arc::new(none),
        Arc::new(String::new()),
        s.cwd.clone(),
        found.workspace,
    );
    let shared = AgentDeps {
        spill_dir: Some(spill),
        hooks: host.clone(),
        ledger: Some(ledger.clone()),
        web_search: s.web_search_config(),
        ..empty
    };
    let agents = Agents {
        host: host.clone(),
        deps: shared.clone(),
        tools: wrapped.clone(),
        prompt: found.prompt.clone(),
        mods: mods.clone(),
        inbox: sender,
    }
    .hub();
    let main_hub = s.agents.then(|| agents.clone());
    let tools = Arc::new(MainTools::new(wrapped, main_hub, s.deferred_tools));
    let main_prompt = AgentPrompt {
        prompt: found.prompt,
        model: None,
        tools: tools.clone(),
    };
    let deps = AgentDeps {
        tools,
        system_prompt: Arc::new(ModSystemPrompt::new(Arc::new(main_prompt), mods.clone())),
        ..shared
    };

    let (model, effort) = start_on(s, session.saved.as_ref(), &opts);
    let mut agent_opts = AgentOptions::new(model);
    agent_opts.effort = effort;
    let resumed = session.saved.is_some();
    if let Some(saved) = session.saved {
        restore(&host, saved, &ledger, &board, &mut agent_opts);
    }
    let agent = Agent::new(deps, agent_opts);
    host.set_model(&agent.model_spec);
    host.set_effort(agent.effort);
    if resumed {
        record_given(&host, &agent, &opts);
    } else {
        record_start(&host, &agent);
    }
    Ok(Runtime {
        agent,
        host,
        ui,
        fs,
        board,
        skills: found.skills,
        skill_roots: found.skill_roots,
        mcp,
        mods,
        inbox,
        agents,
        jobs,
    })
}
