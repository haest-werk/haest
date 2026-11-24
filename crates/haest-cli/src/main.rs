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
//!   haest acp                 an editor's agent (Zed, JetBrains, any ACP client) over stdin and stdout
//!
//! Unix only (macOS, Linux): the terminal, the workspace's process groups
//! and the control socket are Unix's.

#[cfg(not(unix))]
compile_error!(
    "haest-cli runs on Unix only (macOS, Linux): its terminal, process groups and control socket"
);

mod acp;
mod agents;
mod args;
mod ask;
mod changes;
mod commands;
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
    io::{IsTerminal, Read, Write},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use haest::{
    config::{BrokerAccess, ConfigError, ControlSocket, Loaded, Loader, env_var},
    core::{ModelError, error::report, session::sessions_dir},
    runtime::{BuildError, SessionFileError, Ui, list_sessions},
    session::{Hub, HubOptions, SessionPrompter, start},
    settings::Settings,
    transport::{Attached, Broker, Ended, Grant, Stdio, UnixSocket},
    wire::{Level, Origin, encode_line},
    workspace::RealFs,
};
use tokio_util::sync::CancellationToken;

use crate::{
    args::{Args, Command},
    changes::Sandbox,
    commands::Outcome,
    mods::{CliPrompter, TermPrompter},
    run::{Brakes, Session},
    term::{Runtime, TermUi},
};

#[expect(clippy::print_stderr, reason = "the CLI's own error line")]
fn fail(code: u8, msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("haest: {}", render::inert(&msg.to_string()));
    ExitCode::from(code)
}

#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a subcommand answers on the terminal"
)]
async fn subcommand(cmd: Command, loaded: &Loaded) -> ExitCode {
    let s = &loaded.config.settings;
    match cmd {
        Command::Models => print!("{}", models::render(haest::providers::Catalog::builtin())),
        Command::Sessions { here } => {
            let dir = sessions_dir(&s.data_dir);
            let rows: Vec<String> = list_sessions(&RealFs, &dir)
                .await
                .iter()
                .filter(|info| !here || info.cwd == s.cwd)
                .map(|info| session::format_row(info, !here))
                .collect();
            if rows.is_empty() {
                eprintln!("No saved sessions{}.", if here { " here" } else { "" });
            }
            rows.iter().for_each(|r| println!("{r}"));
        }
        Command::Settings => show::settings_report(loaded)
            .iter()
            .for_each(|l| println!("{}", render::inert(l))),
        Command::Ctl(cmd) => return ctl::run(cmd, &loaded.config.control_dir()).await,
        // Served before the load (main): each session loads its own directory's.
        Command::Acp => unreachable!("haest acp is served before the load"),
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

/// What a `-p` run that ends with changes only in the sandbox says to do.
const PRINT_HINT: &str =
    "Run with -w real to edit the project directly, or interactively and /apply.";

/// `input` asked once ([`print_input`], read before the build).
async fn print_mode(
    session: &Session,
    brakes: &Brakes,
    input: &str,
    ui: &TermUi,
    sandbox: Option<&Sandbox>,
) -> ExitCode {
    let input = input.to_string();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let outcome = run::unless_interrupted(
        &cancel,
        session.with(move |rt| Box::pin(async move { commands::resolve(rt, &input, &stop).await })),
    )
    .await;
    let code = match outcome {
        None => {
            brakes.warn("Stopped the command.");
            ExitCode::from(130)
        }
        Some(Ok(Outcome::Send(text))) => match run::human(session, text).await {
            Some(end) => {
                // Interrupted: the human wants out, not the background work's answers.
                if end.stop_reason != haest::StopReason::Aborted {
                    run::settle(session, brakes).await;
                }
                run::exit_code(&end)
            }
            None => ExitCode::FAILURE,
        },
        Some(Ok(Outcome::Done | Outcome::Exit)) => ExitCode::SUCCESS,
        Some(Err(_)) => ExitCode::FAILURE,
    };
    let asked = session
        .with(|rt| {
            Box::pin(async move {
                if let Some(lost) = changes::unapplied(rt).await {
                    rt.hooks().warn(&format!("{lost}\n{PRINT_HINT}"));
                }
            })
        })
        .await;
    // A peer closed the conversation first. The sandbox is still haest's,
    // and is asked once the stream has ended, when nothing changes it more.
    if asked.is_err()
        && let Some(sandbox) = sandbox
    {
        ui.shown().await;
        if let Some(lost) = sandbox.unapplied().await {
            let text = format!("{lost}\n{PRINT_HINT}");
            ui.tell_after_the_end(Level::Warning, &text).await;
        }
    }
    code
}

/// Driven by the parent process over stdin and stdout, as a person at a
/// terminal would drive it, until stdin closes (once what it asked has
/// finished) or it sends `close`.
async fn stdio_mode(session: &Session) -> ExitCode {
    let attached = Stdio::process().attach(session.clone(), Grant::command(Origin::Human));
    match attached.ended().await {
        Ended::PeerLeft | Ended::Closed => ExitCode::SUCCESS,
        Ended::Failed(e) => fail(1, format!("reading stdin: {}", report(&e))),
        other => fail(1, format!("the connection ended: {other}")),
    }
}

/// A `--stdio` run is over, its conversation closed: what the sandbox still
/// holds goes when haest exits, now, and the parent cannot apply it (the
/// wire has no /apply). So it is said, as print mode says it: on stdout as
/// the stream's last frame (the connection has ended, so it is written
/// here) and on stderr. Asked once the conversation has closed, so nothing
/// changes the sandbox after.
async fn stdio_lost(ui: &TermUi, sandbox: &Sandbox) {
    let Some(lost) = sandbox.unapplied().await else {
        return;
    };
    let text = format!("{lost}\nRun with -w real to edit the project directly.");
    if let Some(frame) = ui.last_notice(Level::Warning, &text).await {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(encode_line(&*frame).as_bytes());
        let _ = out.flush();
    }
    let _ = writeln!(std::io::stderr().lock(), "{}", render::inert(&text));
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
        ControlSocket::Command => Grant::command(Origin::Agent),
        // Watch, and a level this command does not know yet: the least access.
        _ => Grant::watch(Origin::Agent),
    };
    let sid = &session.meta().sid;
    match UnixSocket::new(&dir).attach(session.clone(), grant) {
        Ok(attached) => {
            let id = &sid[..8.min(sid.len())];
            ui.info(&format!(
                "Control socket open ({open}): haest ctl events {id} --follow"
            ));
            Some(attached)
        }
        Err(e) => {
            ui.warn(&format!("No control socket: {}", report(&e)));
            None
        }
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
///
/// The "Broker link" line is said once the link is set up, and the dial
/// already runs on a task of its own then: a broker may have its catch-up
/// answered before the line is in the stream. A broker that follows the
/// stream gets the line either way, in the catch-up or after it.
fn broker_link(
    session: &Session,
    (url, access, token): BrokerLink,
    ui: &Arc<TermUi>,
) -> Option<tokio::task::JoinHandle<()>> {
    let url = url.filter(|u| !u.trim().is_empty())?;
    let grant = match access {
        BrokerAccess::Command => Grant::command(Origin::Agent),
        // Watch, and a level this command does not know yet: the least access.
        _ => Grant::watch(Origin::Agent),
    };
    // Said without its credentials: the URL is not where a secret goes,
    // but one may be there.
    let shown = haest::http::redacted_url(&url).into_owned();
    let mut link = Broker::new(url);
    if let Some(token) = token {
        link = link.with_token(token);
    }
    match link.attach(session.clone(), grant) {
        Ok(attached) => {
            ui.info(&format!("Broker link: {shown} ({access})"));
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

/// A peer may answer approval asks: the control socket or the broker link
/// gives `command` access. A run at a terminal then asks both.
fn peers_answer(control: ControlSocket, (url, access, _): &BrokerLink) -> bool {
    let broker = url.as_deref().is_some_and(|u| !u.trim().is_empty());
    matches!(control, ControlSocket::Command) || (broker && matches!(access, BrokerAccess::Command))
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

/// What a failed build tells the human; a model it cannot use says what to
/// do about it (a key only when one is missing), and a model or a session
/// that is not there says where to find another.
fn build_failed(e: BuildError) -> String {
    match e {
        BuildError::Model(e) => {
            let next = match &e {
                ModelError::MissingKey { .. } => "Set its key, or pick",
                ModelError::Unavailable { .. } => "Fix the provider's configuration, or pick",
                _ => "Pick",
            };
            format!(
                "{}. {next} another model with -m (haest models lists them).",
                report(&e)
            )
        }
        BuildError::SessionFile(e @ SessionFileError::NotFound { .. }) => {
            format!("{e} (haest sessions lists them)")
        }
        e => report(&e),
    }
}

/// What a failed load tells the human; a setting there is none of says
/// where they are listed: `--help`, which reads no config (`haest settings`
/// loads it, so it fails the same way).
fn load_failed(e: ConfigError) -> String {
    match e {
        e @ ConfigError::UnknownSetting { .. } => format!("{e} (haest --help lists them)"),
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
        None => std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?,
    };
    match std::fs::canonicalize(&dir) {
        Ok(real) if real.is_dir() => Ok(real),
        _ => Err(format!("{} is not a directory", dir.display())),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut cli = match args::parse_from(std::env::args_os()) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    let command = cli.args.command.take();
    let args = &cli.args;
    let env = unicode_vars(std::env::vars_os());
    let cwd = match working_dir(args) {
        Ok(d) => d,
        Err(e) => return fail(2, e),
    };
    let home = env_var(&env, "HOME").unwrap_or("/").to_string();
    if let Some(Command::Acp) = command {
        return acp::serve(env, cli.settings, cwd, home).await;
    }
    let defaults = Settings::bare(cwd, home);
    let loaded = match Loader::standard(defaults, env, cli.settings.clone())
        .load(&RealFs)
        .await
    {
        Ok(l) => l,
        Err(e) => return fail(2, load_failed(e)),
    };
    if let Some(cmd) = command {
        return subcommand(cmd, &loaded).await;
    }
    let stdio = loaded.config.stdio;
    let control = (loaded.config.control_socket, loaded.config.control_dir());
    let broker: BrokerLink = (
        loaded.config.broker_url.clone(),
        loaded.config.broker_access,
        loaded.keys.key("HAEST_BROKER_TOKEN"),
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
    let opts = HubOptions {
        ring: loaded.config.event_ring,
        ..HubOptions::default()
    };
    let hub = Hub::new_with_options(uuid::Uuid::new_v4().to_string(), opts);
    let ui = TermUi::with_hub(args.json, hub.clone());
    let ui = Arc::new(if stdio { ui.over_stdio() } else { ui });
    let shown = (!stdio).then(|| term::show(&ui, hub.subscribe(0)));
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
        let interactive = ask::interactive(args.json);
        let term = TermPrompter::new(ui.clone(), interactive);
        if interactive.is_ok() && peers_answer(control.0, &broker) {
            CliPrompter::Shared(term, SessionPrompter::new(hub.clone()))
        } else {
            CliPrompter::Term(term)
        }
    };
    let opts = cli.build_options();
    let inputs = haest::runtime_inputs(loaded);
    let rt = match haest::runtime::build(inputs, ui.clone(), prompter, opts).await {
        Ok(rt) => rt,
        Err(e) => return ended(hub, shown, 2, build_failed(e)).await,
    };
    let brakes = Arc::new(Brakes::of(&rt));
    // Held here, the sandbox lives until haest exits, also when a peer
    // closes the conversation first.
    let sandbox = Sandbox::of(&rt);
    let session = start(rt, hub.clone());
    let interrupts = run::watch_interrupts(brakes.clone(), hub.subscribe(hub.last_seq()));
    let _control = control_socket(&session, control, &ui);
    let link = broker_link(&session, broker, &ui);
    // The REPL quits with the session held: what waits never starts.
    let (code, held) = match input {
        _ if stdio => (stdio_mode(&session).await, None),
        Some(input) => {
            let code = print_mode(&session, &brakes, &input, &ui, sandbox.as_ref()).await;
            (code, None)
        }
        None => match repl::repl(&session, &ui, args.prompt_text(), sandbox.clone()).await {
            Ok(hold) => (ExitCode::SUCCESS, Some(hold)),
            Err(e) => (fail(1, e), None),
        },
    };
    // What haest stops is said; then the session closes the conversation
    // (MCP servers, the session file) and its stream, which the terminal
    // shows to its end.
    let _ = session
        .with(|rt| Box::pin(async move { shutdown(rt).await }))
        .await;
    session.close().await;
    drop(held);
    if stdio && let Some(sandbox) = &sandbox {
        stdio_lost(&ui, sandbox).await;
    }
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
        let e = BuildError::SessionFile(SessionFileError::NotFound { id: "abc".into() });
        let said = build_failed(e);
        assert!(
            said.contains("abc") && said.contains("haest sessions"),
            "{said}"
        );
    }

    /// Each model error ends with what to do about it: a key only when one
    /// is missing. A refused base URL said "Set its key" with the key set.
    #[test]
    fn a_model_that_cannot_be_used_says_what_to_do_about_it() {
        let unavailable = ModelError::Unavailable {
            provider: "anthropic".into(),
            source: "its base URL has an @ past its host".into(),
        };
        let said = build_failed(BuildError::Model(unavailable));
        assert!(!said.contains("key"), "{said}");
        assert!(
            said.contains("@ past its host") && said.contains("Fix the provider's configuration"),
            "{said}"
        );
        let missing = ModelError::MissingKey {
            provider: "anthropic".into(),
            key: "ANTHROPIC_API_KEY".into(),
        };
        let said = build_failed(BuildError::Model(missing));
        assert!(said.contains("Set its key"), "{said}");
        let unknown = ModelError::UnknownProvider {
            provider: "nope".into(),
            known: vec!["anthropic".into()],
        };
        let said = build_failed(BuildError::Model(unknown));
        assert!(!said.contains("key"), "{said}");
        assert!(said.contains("haest models lists them"), "{said}");
    }

    /// A cause under the error is part of the line.
    #[test]
    fn a_failed_build_reports_every_cause() {
        let source = std::io::Error::other("disk full");
        let path = PathBuf::from("/d/sessions/x.jsonl");
        let e = BuildError::SessionFile(SessionFileError::Create { path, source });
        assert!(build_failed(e).ends_with("x.jsonl: disk full"));
    }
}
