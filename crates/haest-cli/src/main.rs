//! `haest`: the command line agent, a terminal over `haest::runtime`.
//!
//!   haest                     a conversation in this directory
//!   haest "fix the test"      the same, starting with that
//!   haest -p "..." / cmd | haest -p   answer once, print the answer, exit
//!   haest --json -p "..."     every loop event as one JSON line
//!   haest --stdio             driven by the parent process: requests on stdin, frames on stdout
//!   haest -r [id]             continue a session (the latest here without an id)
//!   haest models | sessions   what haest knows / what it has saved
//!   haest settings            every setting, its value and where it came from
//!   haest ctl list | events <id> --follow | send <id> "..."   another haest here, by its control socket

mod agents;
mod args;
mod ask;
mod changes;
mod commands;
#[cfg(unix)]
mod ctl;
mod models;
mod mods;
mod remedy;
mod render;
mod repl;
mod run;
mod session;
mod show;
mod term;

use std::{
    collections::HashMap,
    ffi::OsString,
    io::{IsTerminal, Read},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use haest::{
    config::{BrokerAccess, ControlSocket, Loaded, Loader, env_var},
    core::{error::report, session::sessions_dir},
    runtime::{BuildError, SessionError, Ui, list_sessions},
    session::{Backend, Hub, HubOptions, SessionPrompter, start},
    settings::Settings,
    transport::{Attached, Broker, Ended, Grant, Stdio, Transport},
    wire::Origin,
    workspace::RealFs,
};

#[cfg(unix)]
use haest::transport::UnixSocket;

use crate::{
    args::{Args, Command},
    commands::Outcome,
    mods::{CliPrompter, TermPrompter},
    run::{Brakes, Session},
    term::{Runtime, TermUi},
};

fn fail(code: u8, msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("haest: {msg}");
    ExitCode::from(code)
}

async fn subcommand(cmd: &Command, loaded: &Loaded) -> ExitCode {
    let s = &loaded.config.settings;
    match cmd {
        Command::Models => print!("{}", models::render(haest::providers::Catalog::builtin())),
        Command::Sessions { here } => {
            let dir = sessions_dir(&s.data_dir);
            let cwd = s.cwd.to_string_lossy();
            let rows: Vec<String> = list_sessions(&RealFs, &dir)
                .await
                .iter()
                .filter(|info| !here || info.cwd == cwd)
                .map(|info| session::format_row(info, !here))
                .collect();
            if rows.is_empty() {
                eprintln!("No saved sessions{}.", if *here { " here" } else { "" });
            }
            rows.iter().for_each(|r| println!("{r}"));
        }
        Command::Settings => show::settings_report(loaded)
            .iter()
            .for_each(|l| println!("{l}")),
        #[cfg(unix)]
        Command::Ctl(cmd) => return ctl::run(cmd, &loaded.config.control_dir()).await,
        #[cfg(not(unix))]
        Command::Ctl(_) => return fail(2, "haest ctl needs Unix sockets"),
    }
    ExitCode::SUCCESS
}

/// The prompt words, else everything on stdin.
fn print_input(args: &Args) -> Option<String> {
    args.prompt_text().or_else(|| {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).ok()?;
        (!text.trim().is_empty()).then_some(text)
    })
}

/// `input` asked once ([`print_input`], read before the build).
async fn print_mode(session: &Session, brakes: &Brakes, input: &str) -> ExitCode {
    let input = input.to_string();
    let outcome = session
        .with(move |rt| Box::pin(async move { commands::resolve(rt, &input).await }))
        .await;
    let code = match outcome {
        Ok(Outcome::Send(text)) => match run::human(session, text).await {
            Some(end) => {
                // Interrupted: the human wants out, not the background work's answers.
                if end.stop_reason != haest::StopReason::Aborted {
                    run::settle(session, brakes).await;
                }
                run::exit_code(&end)
            }
            None => ExitCode::FAILURE,
        },
        Ok(Outcome::Done | Outcome::Exit) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    };
    let _ = session
        .with(|rt| {
            Box::pin(async move {
                if let Some(lost) = changes::unapplied(rt).await {
                    rt.hooks().warn(&format!(
                        "{lost}\nRun with -w real to edit the project directly, or interactively and /apply."
                    ));
                }
            })
        })
        .await;
    code
}

/// Driven by the parent process over stdin and stdout, as a person at a
/// terminal would drive it, until stdin closes (once what it asked has
/// finished) or it sends `close`.
async fn stdio_mode(session: &Session) -> ExitCode {
    let backend: Arc<dyn Backend> = Arc::new(session.clone());
    let attached = match Stdio::process().attach(backend, Grant::command(Origin::Human)) {
        Ok(attached) => attached,
        Err(e) => return fail(1, report(&e)),
    };
    match attached.ended().await {
        Ended::PeerLeft | Ended::Closed => ExitCode::SUCCESS,
        Ended::Failed(e) => fail(1, format!("reading stdin: {}", report(&e))),
    }
}

/// The control socket, when the setting opens one: another program here
/// reaches the conversation through it (`haest ctl`), its input another
/// program's. Detached when the returned value drops. One that cannot open
/// is said, and the run goes on without it.
fn control_socket(
    session: &Session,
    (open, dir): (ControlSocket, PathBuf),
    ui: &TermUi,
) -> Option<Attached> {
    let grant = match open {
        ControlSocket::Off => return None,
        ControlSocket::Watch => Grant::watch(Origin::Agent),
        ControlSocket::Command => Grant::command(Origin::Agent),
    };
    #[cfg(unix)]
    {
        let backend: Arc<dyn Backend> = Arc::new(session.clone());
        let sid = backend.meta().sid;
        match UnixSocket::new(&dir).attach(backend, grant) {
            Ok(attached) => {
                let id = &sid[..8.min(sid.len())];
                ui.info(&format!(
                    "Control socket open ({}): haest ctl events {id} --follow",
                    open.as_str()
                ));
                Some(attached)
            }
            Err(e) => {
                ui.warn(&format!("No control socket: {}", report(&e)));
                None
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (session, dir, grant);
        ui.warn("No control socket: it needs Unix sockets.");
        None
    }
}

/// The broker link: `broker_url`, `broker_access`, and the token (the key
/// `HAEST_BROKER_TOKEN`).
type BrokerLink = (Option<String>, BrokerAccess, Option<String>);

/// How long a closing run waits for the broker link to hand over the last
/// frames.
const BROKER_DRAIN: std::time::Duration = std::time::Duration::from_secs(2);

/// The broker link, when `broker_url` names one: a fleet view elsewhere
/// watches the conversation through it (or drives it, with `broker_access =
/// command`), its input another program's. A broker that refuses the token
/// is said and not dialed again; a URL that is not one is said, and the run
/// goes on without the link. It ends when the conversation does.
fn broker_link(
    session: &Session,
    (url, access, token): BrokerLink,
    ui: &Arc<TermUi>,
) -> Option<tokio::task::JoinHandle<()>> {
    let url = url.filter(|u| !u.trim().is_empty())?;
    let grant = match access {
        BrokerAccess::Watch => Grant::watch(Origin::Agent),
        BrokerAccess::Command => Grant::command(Origin::Agent),
    };
    // Said without its query: the URL is not where a secret goes, but one
    // may be there.
    let shown = url.split('?').next().unwrap_or_default().to_string();
    let backend: Arc<dyn Backend> = Arc::new(session.clone());
    match Broker::new(url).token(token).attach(backend, grant) {
        Ok(attached) => {
            ui.info(&format!("Broker link: {shown} ({})", access.as_str()));
            let ui = ui.clone();
            Some(tokio::spawn(async move {
                if let Ended::Failed(e) = attached.ended().await {
                    ui.warn(&format!("{}; haest no longer dials it.", report(&e)));
                }
            }))
        }
        Err(e) => {
            ui.warn(&format!("No broker link: {}", report(&e)));
            None
        }
    }
}

/// `1 background job`, `2 background jobs`.
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// haest is exiting: what it started stopped (the background jobs' process
/// groups gone before the runtime shuts down) and the session on disk, each
/// stop said.
async fn shutdown(rt: &Runtime) {
    let stopped = rt.shut_down().await;
    if stopped.subagents > 0 {
        let line = format!("Stopped {}.", count(stopped.subagents, "subagent"));
        rt.hooks().info(&line);
    }
    if stopped.jobs > 0 {
        let line = format!("Killed {}.", count(stopped.jobs, "background job"));
        rt.hooks().info(&line);
    }
}

/// What a failed build tells the human; a model it cannot use, or a session
/// that is not there, says where to find another.
fn build_failed(e: BuildError) -> String {
    match e {
        BuildError::Model(e) => format!(
            "{}. Set its key, or pick another model with -m (haest models lists them).",
            report(&e)
        ),
        BuildError::Session(e @ SessionError::NotFound { .. }) => {
            format!("{e} (haest sessions lists them)")
        }
        e => report(&e),
    }
}

/// The environment's variables whose name and value are both Unicode; any
/// other is skipped (`std::env::vars` would panic on it).
fn unicode_vars(vars: impl Iterator<Item = (OsString, OsString)>) -> HashMap<String, String> {
    vars.filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// The working directory: `-C`, else the process's; real and a directory.
fn working_dir(args: &Args) -> Result<PathBuf, String> {
    let dir = match &args.cwd {
        Some(d) => d.clone(),
        None => std::env::current_dir().map_err(|_| "the current directory is gone".to_string())?,
    };
    match std::fs::canonicalize(&dir) {
        Ok(real) if real.is_dir() => Ok(real),
        _ => Err(format!("{} is not a directory", dir.display())),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match args::parse_from(std::env::args_os()) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    let args = &cli.args;
    let env = unicode_vars(std::env::vars_os());
    let cwd = match working_dir(args) {
        Ok(d) => d,
        Err(e) => return fail(2, e),
    };
    let home = env_var(&env, "HOME").unwrap_or("/");
    let defaults = Settings::bare(cwd, home);
    let loaded = match Loader::standard(defaults, env, cli.settings.clone())
        .load(&RealFs)
        .await
    {
        Ok(l) => l,
        Err(e) => return fail(2, report(&e)),
    };
    if let Some(cmd) = &args.command {
        return subcommand(cmd, &loaded).await;
    }
    let stdio = loaded.config.stdio;
    let control = (loaded.config.control_socket, loaded.config.control_dir());
    let broker: BrokerLink = (
        loaded.config.broker_url.clone(),
        loaded.config.broker_access,
        loaded
            .keys
            .get("HAEST_BROKER_TOKEN")
            .filter(|t| !t.is_empty())
            .cloned(),
    );
    if stdio && (args.print || args.json || args.prompt_text().is_some()) {
        return fail(
            2,
            "--stdio takes its requests on stdin: no prompt, -p or --json with it",
        );
    }
    let print = !stdio && (args.print || args.json || !std::io::stdin().is_terminal());
    // The stream first: what the build says is in it, and the terminal shows
    // it (over stdio, the parent reads it from the start).
    let ring = HubOptions {
        ring: loaded.config.event_ring,
        ..HubOptions::default()
    };
    let hub = Hub::new(uuid::Uuid::new_v4().to_string(), ring);
    let ui = TermUi::with_hub(args.json, hub.clone());
    let ui = Arc::new(if stdio { ui.over_stdio() } else { ui });
    let shown = (!stdio).then(|| term::show(ui.term.clone(), hub.subscribe(0)));
    loaded.notes.iter().for_each(|n| ui.warn(n));
    // What print mode asks, read before the build: with nothing to ask, the
    // run fails having created no session.
    let input = match print.then(|| print_input(args)) {
        Some(None) => {
            let why = "nothing to ask: give a prompt, or pipe one on stdin";
            return ended(hub, shown, 2, why).await;
        }
        asked => asked.flatten(),
    };
    let prompter = if stdio {
        CliPrompter::Session(SessionPrompter::new(hub.clone()))
    } else {
        CliPrompter::Term(TermPrompter::new(ui.clone(), ask::interactive()))
    };
    let opts = cli.build_options();
    let inputs = haest::runtime_inputs(loaded);
    let rt = match haest::runtime::build(inputs, ui.clone(), prompter, opts).await {
        Ok(rt) => rt,
        Err(e) => return ended(hub, shown, 2, build_failed(e)).await,
    };
    let brakes = Arc::new(Brakes::of(&rt));
    let session = start(rt, hub.clone());
    let interrupts = run::watch_interrupts(brakes.clone(), hub.subscribe(hub.last_seq()));
    let _control = control_socket(&session, control, &ui);
    let link = broker_link(&session, broker, &ui);
    let code = match input {
        _ if stdio => stdio_mode(&session).await,
        Some(input) => print_mode(&session, &brakes, &input).await,
        None => match repl::repl(&session, &ui, args.prompt_text()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(1, e),
        },
    };
    // What haest stops is said; then the session closes the conversation
    // (MCP servers, the session file) and its stream, which the terminal
    // shows to its end.
    let _ = session
        .with(|rt| Box::pin(async move { shutdown(rt).await }))
        .await;
    session.close().await;
    // The link hands the broker the last frames, then ends; a broker that
    // does not read them is not waited for long.
    if let Some(mut link) = link
        && tokio::time::timeout(BROKER_DRAIN, &mut link).await.is_err()
    {
        link.abort();
    }
    if let Some(shown) = shown {
        let _ = shown.await;
    }
    interrupts.abort();
    code
}

/// A run that ends before its session starts: the stream closes, the
/// terminal shows what it holds, then [`fail`] says why.
async fn ended(
    hub: Hub,
    shown: Option<tokio::task::JoinHandle<()>>,
    code: u8,
    why: impl std::fmt::Display,
) -> ExitCode {
    hub.close();
    if let Some(shown) = shown {
        let _ = shown.await;
    }
    fail(code, why)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime names no command; the CLI says where its sessions are listed.
    #[test]
    fn a_session_that_is_not_there_points_at_haest_sessions() {
        let e = BuildError::Session(SessionError::NotFound { id: "abc".into() });
        let said = build_failed(e);
        assert!(
            said.contains("abc") && said.contains("haest sessions"),
            "{said}"
        );
    }

    /// A cause under the error is part of the line.
    #[test]
    fn a_failed_build_reports_every_cause() {
        let source = std::io::Error::other("disk full");
        let path = PathBuf::from("/d/sessions/x.jsonl");
        let e = BuildError::Session(SessionError::Create { path, source });
        assert!(build_failed(e).ends_with("x.jsonl: disk full"));
    }
}
