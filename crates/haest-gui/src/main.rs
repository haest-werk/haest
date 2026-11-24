//! haest-gui: a native window onto the haests running on this machine.

use std::{path::PathBuf, process::ExitCode, time::Duration};

mod app;

use app::{Args, run};

const USAGE: &str = "\
haest-gui: watch and drive the haests running on this machine

Usage: haest-gui [--control-dir DIR] [--replay FILE [--replay-pause-ms N]]

  --control-dir DIR     where the control sockets are (default: haest's own
                        control_dir setting, else <data_dir>/control)
  --replay FILE         play a recording (`haest ctl events <id> --json`) instead
  --replay-pause-ms N   the longest pause between replayed frames (default 400;
                        0 plays it at once)
";

#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the app's usage and its own error line"
)]
fn main() -> ExitCode {
    let mut args = Args {
        replay_pause: Duration::from_millis(400),
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let parsed: Result<(), String> = match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--control-dir" => {
                value("--control-dir").map(|v| args.control_dir = Some(PathBuf::from(v)))
            }
            "--replay" => value("--replay").map(|v| args.replay = Some(PathBuf::from(v))),
            "--replay-pause-ms" => value("--replay-pause-ms").and_then(|v| {
                v.parse::<u64>()
                    .map(|ms| args.replay_pause = Duration::from_millis(ms))
                    .map_err(|_not_a_number| format!("--replay-pause-ms takes a number, not {v}"))
            }),
            other => Err(format!("unknown argument {other}")),
        };
        if let Err(e) = parsed {
            eprintln!("haest-gui: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("haest-gui: {e:#}");
            ExitCode::FAILURE
        }
    }
}
