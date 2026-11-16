//! One conversation built from what the host hands in ([`RuntimeInputs`],
//! [`BuildOptions`]): providers, workspace, tools (MCP servers included),
//! mods, subagents, the inbox and the session file -- resumed or new -- and
//! the agent over all of it ([`build`]).

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use haest_settings::Settings;

use crate::{
    Agents, Runtime, RuntimeHooks, Ui,
    haest::{
        Agent, AgentDeps, AgentOptions, ChildEnv, KeySource, Workspace,
        core::fs::Fs,
        core::ledger::Ledger,
        core::session::SessionRecord,
        core::skills::{Skill, SkillScanRoots, scan_roots, skill_scan_roots, skills_in},
        core::task_board::TaskBoard,
        core::tool::Tool,
        mcp::McpServerConfig,
        mods::{ModSystemPrompt, Prompter},
        now_ms,
        providers::{CustomProviders, ProviderRegistry, RegistryOptions, RoutingEnv},
    },
    inbox, mods,
    prompt::{AgentPrompt, RuntimePrompt, skill_options},
    store::{create, open_session, record_given, record_start, restore, start_on},
    tools::{MainTools, WrappedTools, image_options, own_tools},
    world::{World, connect_mcp, jobs_for, workspace_for},
};

/// Everything a conversation is built from. haest reads no file, variable or
/// flag of its own: what is not in here does not exist for it.
///
/// Start from [`RuntimeInputs::new`] (the settings and the world, nothing
/// else) and set the fields the host has. haest-config's `Loaded` has a field
/// for each; its docs show which goes where.
pub struct RuntimeInputs {
    /// Every setting of the run: which capabilities are on, the limits, the
    /// model, where the sessions go.
    pub settings: Settings,
    /// Where the agent's files are, and whether it may start real processes.
    pub world: World,
    /// Variables the host passes on purpose (haest-config's `env` setting):
    /// every process haest starts gets them over the world's `child_env`, the
    /// sandbox's shell included (it gets nothing else of the host's
    /// environment).
    pub env: ChildEnv,
    /// What the host answers for a provider variable (`OPENROUTER_API_KEY`,
    /// `ANTHROPIC_BASE_URL`, ...): its environment, a vault, a fixed map. A
    /// closure `Fn(&str) -> Option<String>` and a `HashMap<String, String>`
    /// are both a [`KeySource`]. GenerateImage asks it for
    /// `OPENROUTER_API_KEY` too.
    pub keys: Arc<dyn KeySource>,
    /// providers.json's entries, by name: providers beside the built-in ones.
    pub providers: CustomProviders,
    /// The MCP servers to start. In a [`World::Fs`] the stdio ones are skipped
    /// with a warning: they are real processes.
    pub mcp_servers: Vec<McpServerConfig>,
    /// OpenRouter's `provider` routing; None = none goes out.
    pub routing: Option<RoutingEnv>,
}

impl RuntimeInputs {
    /// The settings in this world, and nothing else: no variables passed, no
    /// keys, no custom providers, no MCP servers, no routing.
    ///
    /// With no keys, [`build`] fails unless the model's provider needs none:
    /// set [`keys`](Self::keys) for the providers the settings use.
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

/// What a host decides per run, beside the settings: which session to write,
/// and whether the settings' model and effort were asked for on purpose.
///
/// The default is a new session on the settings' model and effort.
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
    hooks: &Arc<RuntimeHooks>,
    world: &World,
    env: &ChildEnv,
    fs: &Arc<dyn Fs>,
    spill: &Path,
) -> anyhow::Result<Found> {
    let skill_roots = skill_scan_roots(fs.as_ref(), &skill_options(&hooks.settings)).await;
    let skills = Arc::new(Mutex::new(skills_in(fs.as_ref(), &skill_roots).await));
    let skill_dirs = scan_roots(&skill_roots);
    let workspace = workspace_for(&hooks.settings, world, env, &skill_dirs, spill).await?;
    let prompt = Arc::new(RuntimePrompt::new(hooks, fs, &skills, workspace.as_ref()).await);
    Ok(Found {
        skill_roots,
        skills,
        workspace,
        prompt,
    })
}

/// The task list, saved to the session file on every change.
fn task_board(hooks: &Arc<RuntimeHooks>) -> Arc<TaskBoard> {
    let hooks = hooks.clone();
    Arc::new(TaskBoard::new(move |tasks| {
        hooks.append(SessionRecord::Tasks {
            t: now_ms(),
            tasks: tasks.to_vec(),
        })
    }))
}

/// Build the whole conversation from what the host hands in, behind `ui`;
/// `prompter` answers the approval mod's asks.
///
/// It opens the session `opts` names (a new one, the latest in the cwd, or one
/// by id) and puts a resumed one back in place: its history, task list,
/// spend, model and effort (the settings' model or effort instead, when
/// [`BuildOptions`] says the run asked for it). It finds the skills and opens
/// the workspace the settings pick, creates a new session's file with its
/// header, then starts the MCP servers and the mods together, and assembles
/// the tools, the subagent hub, the inbox and the agent over all of it. The
/// session file then holds the model and effort the conversation starts on.
///
/// What goes wrong after that start is told to `ui`, never returned: an MCP
/// server that did not start, a stdio server a [`World::Fs`] cannot run, a mod
/// that did not load, GenerateImage turned on with no key for it, torn lines
/// skipped in a resumed session.
///
/// Needs a tokio runtime: the session file's writer, the servers and the
/// background jobs run on it.
///
/// # Errors
///
/// Whatever can fail does so before any MCP server or mod starts, and before
/// a new session's file is written: a build that fails has started nothing
/// and left no file for a resume to pick up. It fails when:
///
/// - the session to resume is not there, or cannot be read;
/// - the model it would start on cannot be used: its provider is unknown, or
///   the host hands in no key for it (a `ProviderConfigError` from
///   haest-providers, which the error downcasts to);
/// - the workspace cannot open: `workspace = real` in a [`World::Fs`], the
///   spill dir cannot be made, the sandbox fails to open;
/// - a new session's file cannot be created.
///
/// # Examples
///
/// The crate docs build a whole conversation over a host's files.
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
    // The model the conversation starts on is one it can use (its provider
    // known, its key there), or the build fails with nothing written: a run
    // that cannot start leaves no session for a resume to pick up.
    let (model, effort) = start_on(&settings, session.saved.as_ref(), &opts);
    let reg_opts = RegistryOptions {
        routing,
        ..Default::default()
    };
    let registry = ProviderRegistry::new(keys.clone(), providers, reg_opts);
    registry.resolve_spec(&model)?;
    let spill = settings.spill_dir(session.store.session_id());
    let hooks = Arc::new(RuntimeHooks::new(
        settings,
        ui.clone(),
        session.store.clone(),
    ));
    let s = &hooks.settings;

    // Whatever can fail runs before anything starts, so a failed build has
    // started nothing and left no file: the model is checked (above), the
    // files are found, then a new session's file is created. Then the MCP servers and the mods start,
    // together; from there on nothing fails.
    let found = find(&hooks, &world, &env, &fs, &spill).await?;
    if session.saved.is_none() {
        create(&session.store, &s.cwd).await?;
    }
    let (mcp, mods) = tokio::join!(
        connect_mcp(&hooks, &world, &env, mcp_servers),
        mods::load(&hooks, fs.as_ref(), prompter),
    );

    let board = task_board(&hooks);
    let (sender, inbox) = inbox::inbox(&hooks, s.max_agent_turns);
    let jobs = jobs_for(s, sender.clone());
    let image = image_options(s, keys.as_ref()).unwrap_or_else(|why| {
        hooks.warn(&why);
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
    let ledger = Arc::new(Mutex::new(Ledger::new(
        s.conversation_caps(),
        hooks.clone(),
    )));

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
        hooks: hooks.clone(),
        ledger: Some(ledger.clone()),
        web_search: s.web_search_config(),
        ..empty
    };
    let agents = Agents {
        hooks: hooks.clone(),
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

    let mut agent_opts = AgentOptions::new(model);
    agent_opts.effort = effort;
    let resumed = session.saved.is_some();
    if let Some(saved) = session.saved {
        restore(&hooks, saved, &ledger, &board, &mut agent_opts);
    }
    let agent = Agent::new(deps, agent_opts);
    hooks.set_model(&agent.model_spec());
    hooks.set_effort(agent.effort());
    if resumed {
        record_given(&hooks, &agent, &opts);
    } else {
        record_start(&hooks, &agent);
    }
    Ok(Runtime {
        agent,
        hooks,
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
