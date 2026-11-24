//! Opening a session (`session/new`, `session/load`, `session/resume`): the
//! host's inputs for the client's directory with the client's MCP servers
//! and haest-acp's plan mode added, the conversation built behind a session,
//! and a forwarder that tells its stream once the client knows the session.

use std::{
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use agent_client_protocol::{
    Client, Error, ErrorCode,
    v1::{
        ConnectionTo,
        schema::{
            AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate, CurrentModeUpdate,
            McpServer, SessionConfigOption, SessionId, SessionModeState, SessionUpdate,
            UnstructuredCommandInput,
        },
    },
};
use haest_core::{Message, ToolSourceRef, error::report, task_board::Task};
use haest_mods::{PLAN_COMMAND, PlanMod};
use haest_runtime::{BuildError, BuildOptions, HubUi, Resume, Runtime, SessionFileError, build};
use haest_session::{Hub, start};
use tokio::sync::oneshot;

use crate::{
    Setup, config,
    forward::Forwarder,
    mcp,
    permission::AcpPrompter,
    replay::replay,
    state::{Announced, Live, Relay, State},
    tell::Teller,
};

/// The built-in command that compacts the conversation.
pub(crate) const COMPACT: &str = "compact";

/// What the client asked to open.
pub(crate) struct Request {
    pub(crate) cwd: PathBuf,
    pub(crate) mcp_servers: Vec<McpServer>,
    /// A new session, or the saved one to go on with.
    pub(crate) resume: Resume,
    /// Tell the saved history again (`session/load`).
    pub(crate) replay: bool,
}

/// A session built, not yet told the client: the answer's parts, and what
/// follows the answer.
pub(crate) struct Opened {
    pub(crate) live: Arc<Live>,
    pub(crate) modes: SessionModeState,
    pub(crate) options: Vec<SessionConfigOption>,
    tell: Teller,
    commands: Vec<AvailableCommand>,
    ready: oneshot::Sender<()>,
}

impl Opened {
    /// After the answer: the commands, then the stream from its start.
    pub(crate) fn go(self) {
        let update = AvailableCommandsUpdate::new(self.commands);
        self.tell
            .tell(SessionUpdate::AvailableCommandsUpdate(update));
        let _ = self.ready.send(());
    }
}

/// The saved history a load tells again, read before the session starts.
struct Saved {
    history: Vec<Message>,
    tasks: Vec<Task>,
}

/// Open the session `req` asks for on `conn`.
pub(crate) async fn open(
    state: &State,
    conn: ConnectionTo<Client>,
    req: Request,
) -> Result<Opened, Error> {
    if !req.cwd.is_absolute() {
        return Err(Error::invalid_params().data("cwd must be an absolute path"));
    }
    let servers = mcp::servers(req.mcp_servers).map_err(|why| Error::invalid_params().data(why))?;
    let setup = Setup::new(&req.cwd);
    let inputs = state
        .conversations
        .inputs(&setup)
        .await
        .map_err(|e| Error::new(ErrorCode::INTERNAL_ERROR, report(&*e)))?;
    let mut all = inputs.mcp_servers().to_vec();
    all.extend(servers);

    // The prompter and plan mode are made before the conversation, which
    // takes them; they reach the client once it is built.
    let teller: Arc<OnceLock<Teller>> = Arc::default();
    let plan_on = Arc::new(AtomicBool::new(false));
    let plan = PlanMod::new().with_listener({
        let (teller, plan_on) = (teller.clone(), plan_on.clone());
        move |on| {
            plan_on.store(on, Ordering::SeqCst);
            if let Some(tell) = teller.get() {
                let mode = if on {
                    config::PLAN_MODE
                } else {
                    config::DEFAULT_MODE
                };
                tell.tell(SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                    mode,
                )));
            }
        }
    });
    let inputs = inputs.with_mcp_servers(all).with_plan(plan);
    let hub = Hub::new(state.stream_id());
    let sub = hub.subscribe(0);
    let (told, relay) = Relay::channel();
    let announced = Announced::default();
    let prompter = AcpPrompter {
        tell: teller.clone(),
        hub: hub.clone(),
        relay: relay.clone(),
        announced: announced.clone(),
        unnamed: AtomicU64::new(0),
    };
    let opts = BuildOptions {
        resume: req.resume,
        ..BuildOptions::default()
    };
    let rt = build(inputs, HubUi::new(hub.clone()), prompter, opts)
        .await
        .map_err(build_failed)?;

    let id = SessionId::from(rt.session_id());
    let tell = Teller {
        conn,
        session: id.clone(),
        caps: state.caps(),
    };
    let _ = teller.set(tell.clone());
    let tools: ToolSourceRef = rt.agent().deps().tools().clone();
    let board = rt.board().clone();
    let cwd = rt.hooks().settings().cwd.clone();
    let model_spec = rt.agent().model_spec().to_string();
    let effort = rt.agent().effort();
    let commands = commands(&rt);
    let saved = req.replay.then(|| Saved {
        history: rt.agent().history().to_vec(),
        tasks: board.list(),
    });
    let session = start(rt, hub);

    if let Some(saved) = saved {
        replay(&tell, &saved.history, &saved.tasks, &tools, &cwd);
    }
    let (ready, go) = oneshot::channel();
    let shown = (model_spec.clone(), effort.map(|e| e.as_str().to_string()));
    let forwarder = Forwarder::new(tell.clone(), tools, board, cwd, announced, shown);
    let forward = tokio::spawn(forwarder.run(sub, go, told));
    let live = Arc::new(Live {
        id,
        session,
        relay,
        forward,
    });
    Ok(Opened {
        live,
        modes: config::modes(plan_on.load(Ordering::SeqCst)),
        options: config::options(&model_spec, effort),
        tell,
        commands,
        ready,
    })
}

/// Why the conversation could not be built, as the client's error: a saved
/// session that is not there is a resource not found.
fn build_failed(e: BuildError) -> Error {
    let code = match &e {
        BuildError::SessionFile(SessionFileError::NotFound { .. }) => ErrorCode::RESOURCE_NOT_FOUND,
        _ => ErrorCode::INTERNAL_ERROR,
    };
    Error::new(code, report(&e))
}

/// The slash commands the client offers: `/compact`, the mods' commands
/// (`/plan` is one) and the skills a person may call.
fn commands(rt: &Runtime<HubUi>) -> Vec<AvailableCommand> {
    let args =
        |hint: &str| AvailableCommandInput::Unstructured(UnstructuredCommandInput::new(hint));
    let mut out = vec![AvailableCommand::new(
        COMPACT,
        "Summarise the conversation so far, to free its context window",
    )];
    for name in rt.mods().commands() {
        let what = if name == PLAN_COMMAND {
            "Plan mode: haest reads and plans, and changes nothing until /plan off"
        } else {
            "A mod's command"
        };
        out.push(AvailableCommand::new(name, what).input(args("arguments")));
    }
    for skill in rt.skills().list().into_iter().filter(|s| s.user_invocable) {
        let hint = skill.argument_hint.as_deref().unwrap_or("arguments");
        let command = AvailableCommand::new(skill.name.clone(), skill.description.clone());
        out.push(command.input(args(hint)));
    }
    out
}
