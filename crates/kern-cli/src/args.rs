//! The command line: a few flags of the CLI's own, and one flag per setting,
//! built from the settings registry -- `--skill-catalog` is `skill_catalog` in
//! a config file and `KERN_SKILL_CATALOG` in the environment; the flag wins.

use std::{ffi::OsString, path::PathBuf};

use clap::{Arg, ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use kern::settings::{DEFS, Def, Kind};

#[derive(Debug, Parser)]
#[command(
    name = "kern",
    version,
    about = "An agent in your terminal: a model, tools, a workspace.",
    after_help = "Every setting below is also a KERN_* variable and a key in ~/.config/kern/config.toml (flag > variable > profile > files). kern is bare by default: no tools, no context files, no skills until you turn them on (--profile claude turns on everything Claude Code has that kern reads)."
)]
pub struct Args {
    /// What to ask. Without it (and with a terminal on stdin) kern starts a conversation.
    pub prompt: Vec<String>,

    /// Answer once and exit; reads the prompt from stdin when none is given.
    #[arg(short, long)]
    pub print: bool,

    /// Write every loop event as one JSON line on stdout (implies --print).
    #[arg(long)]
    pub json: bool,

    /// Continue a session: its id, or the latest session in this directory when no id is given.
    #[arg(short, long, num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// Work in this directory instead of the current one.
    #[arg(short = 'C', long, global = true)]
    pub cwd: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the models kern knows, with their windows and prices.
    Models,
    /// List saved sessions, newest first.
    Sessions {
        /// Only sessions started in this directory.
        #[arg(long)]
        here: bool,
    },
    /// Show every setting: its value and where it came from.
    Settings,
}

impl Args {
    /// The prompt words joined, or None.
    pub fn prompt_text(&self) -> Option<String> {
        let text = self.prompt.join(" ");
        (!text.trim().is_empty()).then_some(text)
    }
}

/// What the command line said: the CLI's own flags, and `(key, raw)` per setting flag.
#[derive(Debug)]
pub struct Cli {
    pub args: Args,
    pub settings: Vec<(String, String)>,
}

fn setting_arg(def: &Def) -> Arg {
    // Global: `kern settings --profile claude` shows what that run would get.
    let mut arg = Arg::new(def.key)
        .long(def.flag())
        .global(true)
        .help(format!("{} [{}]", def.help, def.env()))
        .help_heading(def.group);
    arg = match def.kind {
        // `--mcp` alone is true; `--mcp=false` turns off what a file turned on.
        // The `=` is required, so a prompt after the flag stays the prompt.
        Kind::Bool => arg
            .num_args(0..=1)
            .require_equals(true)
            .default_missing_value("true")
            .value_name("BOOL"),
        Kind::List(_) => arg.value_name("LIST"),
        Kind::Number => arg.value_name("N"),
        Kind::Choice(_) | Kind::Text => arg.value_name("VALUE"),
    };
    match def.short {
        Some(c) => arg.short(c),
        None => arg,
    }
}

pub fn command() -> clap::Command {
    DEFS.iter()
        .fold(Args::command(), |cmd, d| cmd.arg(setting_arg(d)))
}

fn settings_of(m: &ArgMatches) -> Vec<(String, String)> {
    DEFS.iter()
        .filter_map(|d| {
            m.get_one::<String>(d.key)
                .map(|v| (d.key.to_string(), v.clone()))
        })
        .collect()
}

pub fn parse_from<I, T>(argv: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let m = command().try_get_matches_from(argv)?;
    Ok(Cli {
        args: Args::from_arg_matches(&m)?,
        settings: settings_of(&m),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(argv: &[&str]) -> Cli {
        parse_from(std::iter::once("kern").chain(argv.iter().copied())).unwrap()
    }

    #[test]
    fn every_setting_is_a_kebab_flag_and_only_given_ones_are_passed_on() {
        let c = cli(&[
            "-m",
            "a:b",
            "--skill-catalog",
            "--tools",
            "read,bash",
            "--max-turn-usd",
            "5",
            "fix it",
        ]);
        assert_eq!(
            c.settings,
            [
                ("model".to_string(), "a:b".to_string()),
                ("skill_catalog".into(), "true".into()),
                ("tools".into(), "read,bash".into()),
                ("max_turn_usd".into(), "5".into()),
            ]
        );
        assert_eq!(c.args.prompt_text().as_deref(), Some("fix it"));
        assert!(cli(&[]).settings.is_empty());
    }

    #[test]
    fn a_switch_takes_its_value_after_an_equals_so_the_prompt_stays_the_prompt() {
        let c = cli(&["--mcp", "list files"]);
        assert_eq!(c.settings, [("mcp".to_string(), "true".to_string())]);
        assert_eq!(c.args.prompt, ["list files"]);
        assert_eq!(cli(&["--mcp=false"]).settings[0].1, "false");
    }

    #[test]
    fn setting_flags_and_the_dir_also_follow_a_subcommand() {
        let c = cli(&["settings", "--profile", "claude", "-C", "/tmp"]);
        assert!(matches!(c.args.command, Some(Command::Settings)));
        assert_eq!(c.settings, [("profile".to_string(), "claude".to_string())]);
        assert_eq!(c.args.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn resume_without_an_id_means_the_latest_here() {
        assert_eq!(cli(&["-r"]).args.resume, Some(String::new()));
        assert_eq!(cli(&["--resume", "abc"]).args.resume, Some("abc".into()));
        assert_eq!(cli(&[]).args.resume, None);
    }
}
