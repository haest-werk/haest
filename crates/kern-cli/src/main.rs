//! `kern`: the command line agent, a terminal over `kern::runtime`.
//!
//!   kern                     a conversation in this directory
//!   kern "fix the test"      the same, starting with that
//!   kern -p "..." / cmd | kern -p   answer once, print the answer, exit
//!   kern --json -p "..."     every loop event as one JSON line
//!   kern -r [id]             continue a session (the latest here without an id)
//!   kern models | sessions   what kern knows / what it has saved
//!   kern settings            every setting, its value and where it came from

mod agents;
mod args;
mod ask;
mod changes;
mod commands;
mod models;
mod mods;
mod render;
mod repl;
mod run;
mod session;
mod show;
mod term;

use std::{
    collections::HashMap,
    io::{IsTerminal, Read},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use kern::{
    config::{Loaded, Loader, env_var},
    providers::ProviderConfigError,
    runtime::{RuntimeInputs, Ui, World, sessions, world},
    session::sessions_dir,
    settings::Settings,
    workspace::RealFs,
};

use crate::{
    args::{Args, Command},
    commands::Outcome,
    mods::TermPrompter,
    term::{Runtime, TermUi},
};

fn fail(code: u8, msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("kern: {msg}");
    ExitCode::from(code)
}

async fn subcommand(cmd: &Command, loaded: &Loaded) -> ExitCode {
    let s = &loaded.config.settings;
    match cmd {
        Command::Models => print!("{}", models::render(kern::providers::Catalog::builtin())),
        Command::Sessions { here } => {
            let dir = sessions_dir(&s.data_dir);
            let cwd = s.cwd.to_string_lossy();
            let rows: Vec<String> = sessions::list_sessions(&RealFs, &dir)
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
async fn print_mode(rt: &mut Runtime, input: &str) -> ExitCode {
    let code = match commands::resolve(rt, input).await {
        Outcome::Send(text) => {
            let end = run::human(rt, &text).await;
            // Interrupted: the human wants out, not the background work's answers.
            if end.stop_reason != kern::StopReason::Aborted {
                run::settle(rt).await;
            }
            run::exit_code(&end)
        }
        Outcome::Done | Outcome::Exit => ExitCode::SUCCESS,
    };
    if let Some(lost) = changes::unapplied(rt).await {
        rt.host.warn(&format!(
            "{lost}\nRun with -w real to edit the project directly, or interactively and /apply."
        ));
    }
    code
}

/// `1 background job`, `2 background jobs`.
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// kern is exiting: its subagents stopped and its background jobs killed
/// (their process groups gone before the runtime shuts down), each said.
async fn shutdown(rt: &Runtime) {
    let running = rt.agents.running().len();
    rt.agents.stop_all();
    if running > 0 {
        rt.host
            .info(&format!("Stopped {}.", count(running, "subagent")));
    }
    if let Some(jobs) = &rt.jobs {
        let killed = world::kill_jobs(jobs).await;
        if killed > 0 {
            rt.host
                .info(&format!("Killed {}.", count(killed, "background job")));
        }
    }
}

/// What a failed build tells the human; a model it cannot use says how to get one.
fn build_failed(e: &anyhow::Error) -> String {
    match e.downcast_ref::<ProviderConfigError>() {
        Some(e) => {
            format!("{e}. Set its key, or pick another model with -m (kern models lists them).")
        }
        None => format!("{e:#}"),
    }
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
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = match working_dir(args) {
        Ok(d) => d,
        Err(e) => return fail(2, e),
    };
    let home = env_var(&env, "HOME").unwrap_or("/");
    let defaults = Settings::bare(cwd, home);
    let loaded = match Loader::standard(defaults, &env, &cli.settings)
        .load(&RealFs)
        .await
    {
        Ok(l) => l,
        Err(e) => return fail(2, e),
    };
    if let Some(cmd) = &args.command {
        return subcommand(cmd, &loaded).await;
    }
    let print = args.print || args.json || !std::io::stdin().is_terminal();
    let ui = Arc::new(TermUi::new(args.json));
    loaded.notes.iter().for_each(|n| ui.warn(n));
    // What print mode asks, read before the build: with nothing to ask, the
    // run fails having created no session.
    let input = match print.then(|| print_input(args)) {
        Some(None) => return fail(2, "nothing to ask: give a prompt, or pipe one on stdin"),
        asked => asked.flatten(),
    };
    let prompter = TermPrompter::new(ui.clone(), ask::interactive());
    let opts = cli.build_options();
    let inputs = RuntimeInputs {
        settings: loaded.config.settings,
        world: World::Disk {
            child_env: loaded.child_env,
        },
        env: loaded.env,
        keys: Arc::new(loaded.keys),
        providers: loaded.providers,
        mcp_servers: loaded.mcp_servers,
        routing: loaded.routing,
    };
    let mut rt = match kern::runtime::build(inputs, ui, prompter, opts).await {
        Ok(rt) => rt,
        Err(e) => return fail(2, build_failed(&e)),
    };
    let code = match input {
        Some(input) => print_mode(&mut rt, &input).await,
        None => match repl::repl(&mut rt, args.prompt_text()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(1, e),
        },
    };
    shutdown(&rt).await;
    rt.mcp.close().await;
    rt.host.flush().await;
    code
}
