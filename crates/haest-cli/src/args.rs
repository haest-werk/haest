//! The command line: a few flags of the CLI's own, and one flag per setting,
//! built from the settings registry -- `--skill-catalog` is `skill_catalog` in
//! a config file and `HAEST_SKILL_CATALOG` in the environment; the flag wins.

use std::{ffi::OsString, path::PathBuf};

use clap::{Arg, ArgAction, ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use haest::{
    config::{DEFS, Def, Kind},
    runtime::{BuildOptions, OnResume, Prefer, Resume},
};

#[derive(Debug, Parser)]
#[command(
    name = "haest",
    version,
    about = "An agent in your terminal: a model, tools, a workspace.",
    after_help = "Every setting below is also a HAEST_* variable and a key in ~/.config/haest/config.toml (flag > variable > profile > files). haest is bare by default: no tools, no context files, no skills until you turn them on (--profile claude turns on everything Claude Code has that haest reads)."
)]
pub(crate) struct Args {
    /// What to ask. Without it (and with a terminal on stdin) haest starts a conversation.
    pub prompt: Vec<String>,

    /// Answer once and exit; reads the prompt from stdin when none is given.
    #[arg(short, long)]
    pub print: bool,

    /// Write every loop event as one JSON line on stdout (implies --print).
    #[arg(long)]
    pub json: bool,

    /// Continue a session: its id, or the latest session in this directory when no id is given.
    #[arg(short, long, num_args = 0..=1)]
    #[expect(
        clippy::option_option,
        reason = "clap's shape for a flag whose value is optional: absent, bare, or with an id"
    )]
    pub resume: Option<Option<String>>,

    /// Work in this directory instead of the current one.
    #[arg(short = 'C', long, global = true)]
    pub cwd: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// List the models haest knows, with their windows and prices.
    Models,
    /// List saved sessions, newest first.
    Sessions {
        /// Only sessions started in this directory.
        #[arg(long)]
        here: bool,
    },
    /// Show every setting: its value and where it came from.
    Settings,
    /// Watch or drive a haest running here with its control socket on (the control_socket setting).
    #[command(subcommand)]
    Ctl(Ctl),
}

/// `haest ctl`: another haest on this machine, by its control socket. An id
/// may be shortened to a prefix only it has.
#[derive(Debug, Subcommand)]
pub(crate) enum Ctl {
    /// List the haests running here with a control socket, oldest first.
    List {
        /// One JSON array.
        #[arg(long)]
        json: bool,
    },
    /// Show one conversation now: its model, state, messages and cost.
    Inspect {
        /// Its id, as `haest ctl list` shows it.
        id: String,
        /// The snapshot as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show its stream: what it still holds, then with --follow every new frame.
    Events {
        /// Its id, as `haest ctl list` shows it.
        id: String,
        /// Only frames after this seq.
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Keep showing new frames until the conversation ends (or Ctrl+C).
        #[arg(short, long)]
        follow: bool,
        /// One JSON line per frame.
        #[arg(long)]
        json: bool,
    },
    /// Send it input, as another program's (a command socket only); waits for the turn it joins to end.
    Send {
        /// Its id, as `haest ctl list` shows it.
        id: String,
        /// The text.
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Stop its running turn (a command socket only).
    Interrupt {
        /// Its id, as `haest ctl list` shows it.
        id: String,
    },
}

impl Args {
    /// The prompt words joined, or None.
    pub(crate) fn prompt_text(&self) -> Option<String> {
        let text = self.prompt.join(" ");
        (!text.trim().is_empty()).then_some(text)
    }
}

/// What the command line said: the CLI's own flags, and `(key, raw)` per setting flag.
#[derive(Debug)]
pub(crate) struct Cli {
    pub args: Args,
    pub settings: Vec<(String, String)>,
}

impl Cli {
    /// What this run asks of the build: the session to resume, and whether a
    /// flag picked the model or the effort (a flag wins over a resumed
    /// session's own; a variable or a file does not).
    pub(crate) fn build_options(&self) -> BuildOptions {
        let given = |key: &str| self.settings.iter().any(|(k, _)| k == key);
        // `-r ""` is `-r`, as it always was.
        let resume = match self.args.resume.clone() {
            None => Resume::New,
            Some(id) => id
                .filter(|id| !id.is_empty())
                .map_or(Resume::Latest, Resume::Id),
        };
        let prefer = |key: &str| {
            if given(key) {
                Prefer::Settings
            } else {
                Prefer::Saved
            }
        };
        BuildOptions {
            resume,
            on_resume: OnResume {
                model: prefer("model"),
                effort: prefer("effort"),
            },
        }
    }
}

fn setting_arg(def: &Def) -> Arg {
    // Global: `haest settings --profile claude` shows what that run would get.
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
        Kind::List(_) | Kind::Patterns => arg.value_name("LIST"),
        Kind::Number => arg.value_name("N"),
        Kind::Path => arg.value_name("DIR"),
        Kind::Url => arg.value_name("URL"),
        Kind::Choice(_) | Kind::Text => arg.value_name("VALUE"),
        Kind::Vars => arg.action(ArgAction::Append).value_name("NAME=VALUE"),
        // Each one merges into the ones before it.
        Kind::Table => arg.action(ArgAction::Append).value_name("TOML"),
        // A kind haest-config adds later takes a plain value until named here.
        _ => arg.value_name("VALUE"),
    };
    match def.short {
        Some(c) => arg.short(c),
        None => arg,
    }
}

pub(crate) fn command() -> clap::Command {
    DEFS.iter()
        .fold(Args::command(), |cmd, d| cmd.arg(setting_arg(d)))
}

/// Every value given, a repeated flag's each in its order.
fn settings_of(m: &ArgMatches) -> Vec<(String, String)> {
    DEFS.iter()
        .flat_map(|d| {
            let given = m.get_many::<String>(d.key).into_iter().flatten();
            given.map(|v| (d.key.to_string(), v.clone()))
        })
        .collect()
}

pub(crate) fn parse_from<I, T>(argv: I) -> Result<Cli, clap::Error>
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
        parse_from(std::iter::once("haest").chain(argv.iter().copied())).unwrap()
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
    fn the_env_passthrough_is_one_flag_with_three_words() {
        let c = cli(&["--env-passthrough", "none", "go"]);
        assert_eq!(
            c.settings,
            [("env_passthrough".to_string(), "none".to_string())]
        );
        assert_eq!(c.args.prompt, ["go"]);
    }

    #[test]
    fn env_repeats_in_its_order_and_a_bare_name_is_passed_as_given() {
        let c = cli(&["-E", "A=1", "--env", "B", "-E", "A=2", "go"]);
        let env = [("env", "A=1"), ("env", "B"), ("env", "A=2")];
        assert_eq!(c.settings, env.map(|(k, v)| (k.to_string(), v.to_string())));
        assert_eq!(c.args.prompt, ["go"]);
    }

    #[test]
    fn a_table_flag_repeats_in_its_order() {
        let c = cli(&[
            "--cache",
            "system = \"1h\"",
            "--cache",
            "{ keep_warm = true }",
            "go",
        ]);
        let cache = [
            ("cache", "system = \"1h\""),
            ("cache", "{ keep_warm = true }"),
        ];
        assert_eq!(
            c.settings,
            cache.map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert_eq!(c.args.prompt, ["go"]);
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
        let resume = |argv: &[&str]| cli(argv).build_options().resume;
        assert_eq!(resume(&["-r"]), Resume::Latest);
        assert_eq!(resume(&["--resume", ""]), Resume::Latest);
        assert_eq!(resume(&["--resume", "abc"]), Resume::Id("abc".into()));
        assert_eq!(resume(&[]), Resume::New);
    }

    #[test]
    fn a_model_or_effort_flag_is_one_the_run_asked_for() {
        let on_resume = |model, effort| OnResume { model, effort };
        let o = cli(&["-r", "-m", "a:b"]).build_options();
        assert_eq!(o.resume, Resume::Latest);
        assert_eq!(o.on_resume, on_resume(Prefer::Settings, Prefer::Saved));
        let o = cli(&["-r", "abc", "-e", "low"]).build_options();
        assert_eq!(o.on_resume, on_resume(Prefer::Saved, Prefer::Settings));
        let o = cli(&["-r"]).build_options();
        assert_eq!(o.on_resume, on_resume(Prefer::Saved, Prefer::Saved));
    }
}
