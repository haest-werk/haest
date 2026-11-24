//! One command in the simulated shell. Every call gets a FRESH shell over the
//! shared file system: variables, functions and `cd` do not outlive the call,
//! files do.

use std::{
    sync::{Arc, Mutex, MutexGuard, atomic::Ordering},
    time::{Duration, Instant},
};

use bashkit::{Bash, ExecutionLimits, FileSystem, LimitExceeded};
use haest_core::{
    ChildEnv,
    fs::FsRef,
    shell_command::ShellCommandRef,
    workspace::{ExecOptions, ExecResult},
};

use super::commands::HostCommand;
use crate::MAX_OUTPUT_BYTES;

/// The longest one call runs by default, whatever its caller asks
/// ([`SandboxWorkspace::with_max_timeout`](crate::SandboxWorkspace::with_max_timeout)
/// sets another): it bounds a runaway construct.
pub const MAX_SHELL_TIMEOUT: Duration = Duration::from_secs(600);
/// The most [`MAX_SHELL_TIMEOUT`]'s replacement may be: a year keeps every
/// deadline one an `Instant` holds.
pub(crate) const LONGEST_SHELL_TIMEOUT: Duration = Duration::from_secs(365 * 24 * 3600);
/// Loop and command budgets per call, generous enough for real scripts.
const MAX_COMMANDS: usize = 100_000;
const MAX_LOOP_ITERATIONS: usize = 100_000;
/// The shell's user: `$HOME` is `/home/user`, which the scratch disk holds.
const USER: &str = "user";
/// Shell conventions for a stopped command.
const EXIT_TIMEOUT: i32 = 124;
const EXIT_INTERRUPTED: i32 = 130;

/// What a sandbox runs one call over: the shell's file system, the same files
/// as haest sees them, the host's extra commands, the variables it passes and
/// the longest a call runs.
pub(crate) struct Shell<'a> {
    pub fs: Arc<dyn FileSystem>,
    pub haest_fs: &'a FsRef,
    pub commands: &'a [ShellCommandRef],
    pub env: &'a ChildEnv,
    pub max_timeout: Duration,
}

fn shell(world: &Shell<'_>, opts: &ExecOptions, ends_at: Instant) -> Bash {
    let mut builder = Bash::builder().fs(world.fs.clone());
    for (name, value) in world.env.vars() {
        builder = builder.env(name, value);
    }
    for command in world.commands {
        let builtin = HostCommand {
            command: command.clone(),
            fs: world.haest_fs.clone(),
            cancel: opts.cancel.clone(),
            ends_at,
        };
        builder = builder.builtin(command.name(), Box::new(builtin));
    }
    // bashkit sets `$USER` from the user name at build: a host's own wins.
    builder
        .cwd(opts.cwd.clone())
        .username(world.env.get("USER").unwrap_or(USER))
        .limits(
            ExecutionLimits::new()
                .timeout(world.max_timeout)
                .max_commands(MAX_COMMANDS)
                .max_loop_iterations(MAX_LOOP_ITERATIONS)
                .max_stdout_bytes(MAX_OUTPUT_BYTES)
                .max_stderr_bytes(MAX_OUTPUT_BYTES),
        )
        .build()
}

type Sink = Arc<Mutex<(Vec<u8>, Vec<u8>)>>;

/// The streamed copies of stdout and stderr. The lock is held only to copy
/// bytes in or out, so a poisoned one is a bug here.
fn streamed(s: &Sink) -> MutexGuard<'_, (Vec<u8>, Vec<u8>)> {
    s.lock().expect("the streamed output's lock poisoned")
}

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
    // The shell stops at its max timeout whatever the caller asked, and a
    // longer timeout (`Duration::MAX` for "none") would overflow an `Instant`.
    let timeout = opts.timeout.min(world.max_timeout);
    let started = Instant::now();
    let mut bash = shell(&world, &opts, started + timeout);
    // A CPU-bound script never yields to the select below, so the deadline and
    // the cancel ALSO raise the shell's own flag from another task: it stops at
    // the next command. Held in a set, never detached: a dropped call drops
    // the set, which aborts it.
    let flag = bash.cancellation_token();
    let mut watcher = tokio::task::JoinSet::new();
    watcher.spawn({
        let cancel = opts.cancel.clone();
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
    let into = sink.clone();
    let mut options = bashkit::ExecOptions::new()
        .timeout(timeout)
        .streaming(Box::new(move |out, err| {
            let mut s = streamed(&into);
            s.0.extend_from_slice(out.as_bytes());
            s.1.extend_from_slice(err.as_bytes());
        }));
    if !positional.is_empty() {
        options = options.positional(positional.iter().cloned());
    }

    let outcome = tokio::select! {
        r = bash.exec_with_options(script, options) => Ok(r),
        _ = opts.cancel.cancelled() => Err(Stopped::Cancelled),
        _ = tokio::time::sleep(timeout) => Err(Stopped::TimedOut),
    };
    watcher.abort_all();
    let timed_out = started.elapsed() >= timeout;
    let (out, err) = std::mem::take(&mut *streamed(&sink));
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
