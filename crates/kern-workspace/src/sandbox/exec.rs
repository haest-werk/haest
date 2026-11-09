//! One command in the simulated shell. Every call gets a FRESH shell over the
//! shared file system: variables, functions and `cd` do not outlive the call,
//! files do.

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use bashkit::{Bash, ExecutionLimits, FileSystem, LimitExceeded};
use kern_core::{
    fs::Fs,
    shell_command::ShellCommand,
    workspace::{ExecOptions, ExecResult},
};

use super::commands::HostCommand;

/// The per-call timeout is the caller's; this only bounds a runaway construct.
const HARD_DEADLINE: Duration = Duration::from_secs(600);
/// Loop and command budgets per call, generous enough for real scripts.
const MAX_COMMANDS: usize = 100_000;
const MAX_LOOP_ITERATIONS: usize = 100_000;
/// Output kept per stream; the tools cut it down to their own budget.
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
/// The shell's user: `$HOME` is `/home/user`, which the scratch disk holds.
const USER: &str = "user";
/// Shell conventions for a stopped command.
const EXIT_TIMEOUT: i32 = 124;
const EXIT_INTERRUPTED: i32 = 130;

/// What a sandbox runs one call over: the shell's file system, the same files
/// as kern sees them, and the host's extra commands.
pub(crate) struct Shell<'a> {
    pub fs: Arc<dyn FileSystem>,
    pub kern_fs: &'a Arc<dyn Fs>,
    pub commands: &'a [Arc<dyn ShellCommand>],
}

fn shell(world: &Shell<'_>, opts: &ExecOptions, ends_at: Instant) -> Bash {
    let mut builder = Bash::builder().fs(world.fs.clone());
    for command in world.commands {
        let builtin = HostCommand {
            command: command.clone(),
            fs: world.kern_fs.clone(),
            cancel: opts.cancel.clone(),
            ends_at,
        };
        builder = builder.builtin(command.name(), Box::new(builtin));
    }
    builder
        .cwd(opts.cwd.clone())
        .username(USER)
        .limits(
            ExecutionLimits::new()
                .timeout(HARD_DEADLINE)
                .max_commands(MAX_COMMANDS)
                .max_loop_iterations(MAX_LOOP_ITERATIONS)
                .max_stdout_bytes(MAX_OUTPUT_BYTES)
                .max_stderr_bytes(MAX_OUTPUT_BYTES),
        )
        .build()
}

type Sink = Arc<Mutex<(Vec<u8>, Vec<u8>)>>;

/// How a call ended when it did not finish on its own.
enum Stopped {
    Cancelled,
    TimedOut,
}

/// Run `script` (with `positional` as `$1...`) and report it the way a real shell would.
pub(crate) async fn exec(
    world: Shell<'_>,
    script: &str,
    positional: &[String],
    opts: ExecOptions,
) -> ExecResult {
    let started = Instant::now();
    let mut bash = shell(&world, &opts, started + opts.timeout);
    // A CPU-bound script never yields to the select below, so the deadline and
    // the cancel ALSO raise the shell's own flag from another task: it stops at
    // the next command.
    let flag = bash.cancellation_token();
    let watcher = tokio::spawn({
        let (cancel, timeout) = (opts.cancel.clone(), opts.timeout);
        async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = tokio::time::sleep(timeout) => {}
            }
            flag.store(true, Ordering::Relaxed);
        }
    });
    // Streamed copies of the output: what a stopped call returns.
    let sink: Sink = Arc::default();
    let streamed = sink.clone();
    let mut options = bashkit::ExecOptions::new()
        .timeout(opts.timeout)
        .streaming(Box::new(move |out, err| {
            let mut s = streamed.lock().unwrap();
            s.0.extend_from_slice(out.as_bytes());
            s.1.extend_from_slice(err.as_bytes());
        }));
    if !positional.is_empty() {
        options = options.positional(positional.iter().cloned());
    }

    let outcome = tokio::select! {
        r = bash.exec_with_options(script, options) => Ok(r),
        _ = opts.cancel.cancelled() => Err(Stopped::Cancelled),
        _ = tokio::time::sleep(opts.timeout) => Err(Stopped::TimedOut),
    };
    watcher.abort();
    let timed_out = started.elapsed() >= opts.timeout;
    let (out, err) = std::mem::take(&mut *sink.lock().unwrap());
    let partial = |extra: &str, exit_code: i32| ExecResult {
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned() + extra,
        exit_code,
        timed_out,
    };
    match outcome {
        Ok(Ok(r)) => ExecResult {
            stdout: r.stdout.text_lossy().into_owned(),
            stderr: r.stderr.text_lossy().into_owned(),
            exit_code: r.exit_code,
            timed_out: false,
        },
        Ok(Err(bashkit::Error::ResourceLimit(LimitExceeded::Timeout(_))))
        | Err(Stopped::TimedOut) => ExecResult {
            timed_out: true,
            ..partial("", EXIT_TIMEOUT)
        },
        Ok(Err(bashkit::Error::Cancelled)) | Err(Stopped::Cancelled) => partial(
            "",
            if timed_out {
                EXIT_TIMEOUT
            } else {
                EXIT_INTERRUPTED
            },
        ),
        Ok(Err(e @ bashkit::Error::Parse { .. })) => partial(&format!("bash: {e}\n"), 2),
        Ok(Err(e)) => partial(&format!("bash: {e}\n"), 1),
    }
}
