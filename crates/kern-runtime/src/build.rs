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
        Agent, AgentDeps, AgentOptions, KeySource, Workspace,
        fs::Fs,
        ledger::Ledger,
        mcp::McpServerConfig,
        mods::{ModSystemPrompt, Prompter},
        now_ms,
        providers::{CustomProviders, ProviderRegistry, RegistryOptions, RoutingEnv},
        session::SessionRecord,
        skills::{Skill, SkillOpts, scan_roots, skill_scan_roots, skills_in},
        task_board::TaskBoard,
        tool::Tool,
    },
    mods,
    prompt::{AgentPrompt, RuntimePrompt, skill_opts},
    store::{init_store, open_session, restore},
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

/// What a host decides per run, beside the settings.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// None = a new session; Some("") = the latest one in the cwd; Some(id) = that one.
    pub resume: Option<String>,
}

/// What the conversation finds in its world: the skills, the workspace (its
/// sandbox mounts the skill dirs, worked out once with them), the prompt's
/// fixed parts.
struct Found {
    skill_opts: SkillOpts,
    skills: Arc<Mutex<Vec<Skill>>>,
    workspace: Arc<dyn Workspace>,
    prompt: Arc<RuntimePrompt>,
}

async fn find(
    host: &Arc<Host>,
    world: &World,
    fs: &Arc<dyn Fs>,
    spill: &Path,
) -> anyhow::Result<Found> {
    let skill_opts = skill_opts(&host.settings);
    let roots = skill_scan_roots(fs.as_ref(), &skill_opts).await;
    let skills = Arc::new(Mutex::new(skills_in(&roots, fs.as_ref()).await));
    let workspace = workspace_for(&host.settings, world, &scan_roots(&roots), spill).await?;
    let prompt = Arc::new(RuntimePrompt::new(host, fs, &skills, workspace.as_ref()).await);
    Ok(Found {
        skill_opts,
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
        keys,
        providers,
        mcp_servers,
        routing,
    } = inputs;
    let fs = world.fs();
    let (session_id, store, resuming) =
        open_session(&fs, &settings, opts.resume.as_deref(), ui.clone()).await?;
    let host = Arc::new(Host::new(settings, ui.clone(), store));
    let s = &host.settings;
    let spill = s.spill_dir(&session_id);

    // The MCP servers and the mods start while the files are found.
    let (mcp, mods, found) = tokio::join!(
        connect_mcp(&host, &world, mcp_servers),
        mods::load(&host, fs.as_ref(), prompter),
        find(&host, &world, &fs, &spill),
    );
    let found = match found {
        Ok(found) => found,
        Err(e) => {
            mcp.close().await;
            return Err(e);
        }
    };

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

    let mut agent_opts = AgentOptions::new(s.model.clone());
    agent_opts.effort = s.effort;
    if resuming {
        restore(&host, &ledger, &board, &mut agent_opts).await?;
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
        skills: found.skills,
        skill_opts: found.skill_opts,
        mcp,
        mods,
        session_id,
        inbox,
        agents,
        jobs,
    })
}
