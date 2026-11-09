//! `kern`: the command line agent.
//!
//!   kern                     a conversation in this directory
//!   kern "fix the test"      the same, starting with that
//!   kern -p "..." / cmd | kern -p   answer once, print the answer, exit
//!   kern --json -p "..."     every loop event as one JSON line
//!   kern -r [id]             continue a session (the latest here without an id)
//!   kern models | sessions   what kern knows / what it has saved

mod args;
mod ask;
mod changes;
mod commands;
mod config;
mod host;
mod models;
mod mods;
mod prompt;
mod render;
mod repl;
mod run;
mod session;
mod setup;
mod tools;
mod world;

use std::{
    io::{IsTerminal, Read},
    process::ExitCode,
    sync::{Arc, Mutex},
};

use clap::Parser;

use crate::{
    args::{Args, Command},
    commands::Outcome,
    config::{Config, Env},
    render::Renderer,
    setup::Runtime,
};

fn fail(code: u8, msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("kern: {msg}");
    ExitCode::from(code)
}

fn subcommand(cmd: &Command, cfg: &Config) -> ExitCode {
    match cmd {
        Command::Models => print!("{}", models::render(kern::providers::Catalog::builtin())),
        Command::Sessions { here } => {
            let dir = session::sessions_dir(&cfg.data_dir);
            let cwd = cfg.cwd.to_string_lossy();
            let rows: Vec<String> = session::list_sessions(&dir)
                .iter()
                .filter(|s| !here || s.cwd == cwd)
                .map(|s| session::format_row(s, !here))
                .collect();
            if rows.is_empty() {
                eprintln!("No saved sessions{}.", if *here { " here" } else { "" });
            }
            rows.iter().for_each(|r| println!("{r}"));
        }
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

async fn print_mode(rt: &mut Runtime, args: &Args) -> ExitCode {
    let Some(input) = print_input(args) else {
        return fail(2, "nothing to ask: give a prompt, or pipe one on stdin");
    };
    let code = match commands::resolve(rt, &input).await {
        Outcome::Send(text) => run::exit_code(&run::human(rt, &text).await),
        Outcome::Done | Outcome::Exit => ExitCode::SUCCESS,
    };
    if let Some(lost) = changes::unapplied(rt).await {
        rt.host.warn(&format!(
            "{lost}\nRun with -w real to edit the project directly, or interactively and /apply."
        ));
    }
    code
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let env: Env = std::env::vars().collect();
    let Ok(cwd) = std::env::current_dir() else {
        return fail(2, "the current directory is gone");
    };
    let mut cfg = match Config::from(&args, &env, cwd) {
        Ok(c) => c,
        Err(e) => return fail(2, e),
    };
    match std::fs::canonicalize(&cfg.cwd) {
        Ok(dir) if dir.is_dir() => cfg.cwd = dir,
        _ => return fail(2, format!("{} is not a directory", cfg.cwd.display())),
    }
    if let Some(cmd) = &args.command {
        return subcommand(cmd, &cfg);
    }
    let print = args.print || args.json || !std::io::stdin().is_terminal();
    let term = Arc::new(Mutex::new(Renderer::terminal(args.json)));
    let mut rt = match setup::build(cfg, env, term).await {
        Ok(rt) => rt,
        Err(e) => return fail(2, format!("{e:#}")),
    };
    if let Err(e) = rt.agent.deps().models.resolve(&rt.agent.model_spec) {
        return fail(
            2,
            format!("{e}. Set its key, or pick another model with -m (kern models lists them)."),
        );
    }
    let code = if print {
        print_mode(&mut rt, &args).await
    } else {
        match repl::repl(&mut rt, args.prompt_text()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(1, e),
        }
    };
    rt.mcp.close().await;
    rt.host.flush().await;
    code
}
