//! Building one conversation's runtime from the config: providers, workspace,
//! tools (MCP servers included), the session file -- resumed or new -- and the
//! agent over all of it.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, anyhow, bail};
use kern::{
    Agent, AgentDeps, AgentOptions, Workspace, WorkspaceMode,
    context::{InstructionOpts, PromptInputs, current_git_branch},
    fs::{Fs, ensure_dir},
    ledger::Ledger,
    mcp::{McpHub, load_project_mcp_config},
    providers::{ProviderRegistry, RegistryOptions},
    session::{JsonlSessionFile, SessionRecord, iso_utc},
    skills::{Skill, SkillOpts, discover_skills, skill_roots},
    task_board::TaskBoard,
    workspace::{RealFs, RealWorkspace, SandboxMounts, SandboxWorkspace},
};

use crate::{
    config::{Config, Env},
    host::{Host, Term, now_ms},
    prompt::{CliPrompt, platform},
    session::{latest_in, new_session_id, sessions_dir},
    tools::{CliTools, own_tools},
};

pub struct Runtime {
    pub agent: Agent,
    pub host: Arc<Host>,
    pub fs: Arc<dyn Fs>,
    pub board: Arc<TaskBoard>,
    pub skills: Arc<Mutex<Vec<Skill>>>,
    pub skill_opts: SkillOpts,
    pub mcp: Arc<McpHub>,
    pub session_id: String,
}

/// Which session this run writes: a new one, the one named, or the latest here.
fn session_id_for(cfg: &Config) -> anyhow::Result<(String, bool)> {
    match cfg.resume.as_deref() {
        None => Ok((new_session_id(), false)),
        Some("") => latest_in(&sessions_dir(&cfg.data_dir), &cfg.cwd)
            .map(|id| (id, true))
            .ok_or_else(|| anyhow!("no saved session in {} to resume", cfg.cwd.display())),
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
        .init(id, &host.cfg.cwd.to_string_lossy(), now_ms());
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

async fn workspace_for(
    cfg: &Config,
    fs: &Arc<dyn Fs>,
    skill_opts: &SkillOpts,
    spill: &Path,
) -> anyhow::Result<Arc<dyn Workspace>> {
    if cfg.workspace == WorkspaceMode::Real {
        return Ok(Arc::new(RealWorkspace::new()));
    }
    // The spill dir is mounted read-write, so the path in a spill notice is real.
    ensure_dir(fs.as_ref(), spill).await?;
    let mut mounts = SandboxMounts::new(&cfg.cwd).writable(spill);
    for dir in skill_roots(fs.as_ref(), skill_opts).await {
        if fs.exists(&dir).await {
            mounts = mounts.readable(dir);
        }
    }
    let ws = SandboxWorkspace::open(mounts)
        .await
        .context("opening the sandbox")?;
    Ok(Arc::new(ws))
}

async fn connect_mcp(host: &Host, fs: &dyn Fs, cwd: &Path) -> Arc<McpHub> {
    let hub = Arc::new(McpHub::new());
    match load_project_mcp_config(fs, cwd).await {
        Ok(configs) if !configs.is_empty() => {
            hub.connect(&configs, &|m: &str| kern::tool::ToolHost::log(host, m))
                .await;
            for f in hub.failures() {
                host.warn(&format!("MCP server {} did not start: {}", f.name, f.error));
            }
        }
        Ok(_) => {}
        Err(e) => host.warn(&format!(".mcp.json not loaded: {e}")),
    }
    hub
}

pub async fn build(cfg: Config, env: Env, term: Arc<Mutex<Term>>) -> anyhow::Result<Runtime> {
    let fs: Arc<dyn Fs> = Arc::new(RealFs::new());
    let opts = RegistryOptions {
        openrouter_routing: true,
        ..Default::default()
    };
    let registry = ProviderRegistry::load(fs.as_ref(), &cfg.config_dir, env, opts)
        .await
        .map_err(|e| anyhow!("{}", e.0))?;
    let (session_id, resuming) = session_id_for(&cfg)?;
    let store = Arc::new(open_store(&fs, &cfg.data_dir, &session_id)?);
    if resuming && !store.exists().await {
        bail!("no saved session \"{session_id}\" (kern sessions lists them)");
    }
    let host = Arc::new(Host::new(cfg.clone(), term, store.clone()));
    registry.warnings().iter().for_each(|w| host.warn(w));

    let home = cfg.home.clone();
    let skill_opts = SkillOpts {
        cwd: cfg.cwd.clone(),
        home: home.clone(),
        config_dir: home.join(".claude"),
    };
    let skills = Arc::new(Mutex::new(discover_skills(fs.as_ref(), &skill_opts).await));
    let spill = cfg.spill_dir(&session_id);
    let workspace = workspace_for(&cfg, &fs, &skill_opts, &spill).await?;
    let board = Arc::new(TaskBoard::new({
        let host = host.clone();
        move |tasks| {
            host.append(SessionRecord::Tasks {
                t: now_ms(),
                tasks: tasks.to_vec(),
            })
        }
    }));
    let mcp = connect_mcp(&host, fs.as_ref(), &cfg.cwd).await;
    let own = own_tools(board.clone(), skills.clone(), fs.clone());
    let tools = Arc::new(CliTools::new(own, mcp.clone(), cfg.deferred_tools));
    let note = workspace.prompt_note();
    let base = PromptInputs {
        cwd: cfg.cwd.to_string_lossy().into_owned(),
        platform: platform(),
        date: iso_utc(now_ms())[..10].to_string(),
        git_branch: current_git_branch(fs.as_ref(), &cfg.cwd).await,
        tool_names: tools.names(),
        workspace_note: (!note.trim().is_empty()).then_some(note),
        append_system_prompt: cfg.append_system_prompt.clone(),
        ..Default::default()
    };
    let prompt = CliPrompt {
        fs: fs.clone(),
        host: host.clone(),
        skills: skills.clone(),
        instructions: InstructionOpts {
            cwd: cfg.cwd.clone(),
            home,
            config_dir: None,
        },
        base,
    };
    let ledger = Arc::new(Mutex::new(Ledger::new(cfg.ledger, host.clone())));
    let mut deps = AgentDeps::new(
        Arc::new(registry),
        tools,
        Arc::new(prompt),
        cfg.cwd.clone(),
        workspace,
    );
    deps.spill_dir = Some(spill);
    deps.hooks = host.clone();
    deps.ledger = Some(ledger.clone());

    let mut opts = AgentOptions::new(cfg.model.clone());
    opts.effort = cfg.effort;
    if resuming {
        let s = store.load().await.context("reading the session")?;
        if s.corrupt_lines > 0 {
            host.warn(&format!(
                "Skipped {} torn line(s) in the session file.",
                s.corrupt_lines
            ));
        }
        if let Some(l) = &s.ledger {
            ledger.lock().unwrap().restore(l);
        }
        board.restore(s.tasks.unwrap_or_default());
        opts.model = s.model.unwrap_or(opts.model);
        opts.effort = s.effort.unwrap_or(opts.effort);
        opts.history = s.messages;
    }
    let agent = Agent::new(deps, opts);
    host.set_model(&agent.model_spec);
    if !resuming {
        init_store(&host, &agent, &session_id);
    }
    Ok(Runtime {
        agent,
        host,
        fs,
        board,
        skills,
        skill_opts,
        mcp,
        session_id,
    })
}
