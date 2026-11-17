//! One conversation built from what the host hands in ([`RuntimeInputs`],
//! [`BuildOptions`], in inputs.rs): providers, workspace, tools (MCP servers
//! included), mods, subagents, the inbox and the session file -- resumed or
//! new -- and the agent over all of it ([`build`]).

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use crate::{
    BuildError, BuildOptions, Runtime, RuntimeHooks, RuntimeInputs, RuntimeParent, Ui,
    haest::{
        Agent, AgentDeps, AgentOptions, ChildEnv, SharedDeps, Workspace,
        core::deferred_tools::ToolDeferral,
        core::fs::Fs,
        core::ledger::Ledger,
        core::session::SessionRecord,
        core::skills::{Skill, SkillScanRoots, skills_in},
        core::task_board::TaskBoard,
        core::threshold::Thresholds,
        mods::{ApprovalGate, ModSystemPrompt, Prompter},
        now_ms,
        providers::{ProviderRegistry, RegistryOptions},
        tools::DeferredTools,
    },
    inbox, mods,
    prompt::{AgentPrompt, RuntimePrompt, skill_options},
    store::{create, open_session, record_given, record_start, restore, start_on},
    tools::{MainTools, WrappedTools, image_options, own_tools},
    world::{WorkspaceError, World, connect_mcp, jobs_for, workspace_for},
};

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
    passed_env: &ChildEnv,
    fs: &Arc<dyn Fs>,
    spill: &Path,
) -> Result<Found, WorkspaceError> {
    let skill_roots = SkillScanRoots::find(fs.as_ref(), &skill_options(&hooks.settings)).await;
    let skills = Arc::new(Mutex::new(skills_in(fs.as_ref(), &skill_roots).await));
    let skill_dirs = skill_roots.dirs();
    let remedies = hooks.remedies();
    let workspace = workspace_for(
        &hooks.settings,
        world,
        passed_env,
        &skill_dirs,
        spill,
        remedies,
    )
    .await?;
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
/// and left no file for a resume to pick up. A [`BuildError`]:
///
/// - [`Session`](BuildError::Session): the session to resume is not there,
///   or cannot be read; a new session's file cannot be created;
/// - [`Model`](BuildError::Model): the model it would start on cannot be
///   used: a malformed spec, an unknown provider, no key for it;
/// - [`Workspace`](BuildError::Workspace): `workspace = real` in a
///   [`World::Fs`], a spill dir that cannot be made, a sandbox that fails to
///   open.
///
/// # Examples
///
/// The crate docs build a whole conversation over a host's files.
pub async fn build<U, P>(
    inputs: RuntimeInputs,
    ui: Arc<U>,
    prompter: P,
    opts: BuildOptions,
) -> Result<Runtime<U>, BuildError>
where
    U: Ui + 'static,
    P: Prompter + 'static,
{
    let RuntimeInputs {
        settings,
        world,
        passed_env,
        keys,
        providers,
        mcp_servers,
        routing,
        http,
    } = inputs;
    let fs = world.fs();
    let session = open_session(&fs, &settings, &opts.resume, ui.clone()).await?;
    // The model the conversation starts on is one it can use (its provider
    // known, its key there), or the build fails with nothing written: a run
    // that cannot start leaves no session for a resume to pick up.
    let (model, effort) = start_on(&settings, session.saved.as_ref(), &opts);
    let reg_opts = RegistryOptions::default()
        .with_http(http.clone())
        .with_routing(routing);
    let registry = ProviderRegistry::new(keys.clone())
        .with_custom(providers)
        .with_options(reg_opts);
    registry.resolve(&model)?;
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
    let found = find(&hooks, &world, &passed_env, &fs, &spill).await?;
    if session.saved.is_none() {
        create(&session.store, &s.cwd).await?;
    }
    let gate = ApprovalGate::new(prompter);
    let (mcp, mods) = tokio::join!(
        connect_mcp(&hooks, &world, &passed_env, mcp_servers),
        mods::mods_for(&hooks, fs.as_ref(), &gate),
    );

    let board = task_board(&hooks);
    let (sender, inbox) = inbox::inbox(&hooks, s.max_agent_turns);
    let jobs = jobs_for(s, sender.clone());
    let image = match image_options(s, keys.as_ref()) {
        Ok(opts) => opts.map(|o| o.with_http(http)),
        Err(e) => {
            hooks.warn(&format!("{e}."));
            None
        }
    };
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
        s.code.then(|| s.code_tools.clone()),
    ));
    let ledger = Ledger::new(s.conversation_caps(), hooks.clone()).with_remedies(hooks.remedies());

    // What every agent of the conversation runs on; tools and prompt are each
    // agent's own (the hub gives every subagent its own, main's hold the hub).
    let shared = SharedDeps::new(registry, s.cwd.clone(), found.workspace)
        .with_spill_dir(spill)
        .with_hooks(hooks.clone())
        .with_services(hooks.clone())
        .with_facts(hooks.clone())
        .with_turn_caps(hooks.clone())
        .with_ledger(ledger.clone())
        .with_web_search(s.web_search_options())
        .with_remedies(hooks.remedies())
        .with_thresholds(Thresholds::default().wind_down(s.wind_down_at));
    let subagents = RuntimeParent {
        hooks: hooks.clone(),
        deps: shared.clone(),
        tools: wrapped.clone(),
        prompt: found.prompt.clone(),
        mods: mods.clone(),
        inbox: sender,
    }
    .hub();
    let main_hub = s.agents.then(|| subagents.clone());
    let tools = Arc::new(DeferredTools::new(
        MainTools::new(wrapped, main_hub),
        ToolDeferral::new(s.deferred_tools),
    ));
    let deferral = tools.deferral().clone();
    let main_prompt = AgentPrompt {
        prompt: found.prompt,
        model_spec: None,
        tools: tools.clone(),
    };
    let system_prompt = ModSystemPrompt::new(Arc::new(main_prompt), mods.clone());
    let deps = AgentDeps::from_shared(shared, tools, system_prompt);

    let mut agent_opts = AgentOptions::new(model);
    agent_opts.effort = effort;
    let resumed = session.saved.is_some();
    if let Some(saved) = session.saved {
        restore(&hooks, saved, &ledger, &board, &mut agent_opts);
        // The deferred tools it was using stay loaded: no new ToolSearch for them.
        deferral.load_called(&agent_opts.history);
    }
    let agent = Agent::new(deps, agent_opts);
    hooks.set_model_spec(agent.model_spec());
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
        gate,
        inbox,
        subagents,
        jobs,
    })
}
