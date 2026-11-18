//! Running a real program. Children get their own process group so a timeout
//! or an interrupt kills the whole tree (`npm run dev` spawning node spawning
//! esbuild), not just bash.

use std::{
    io, panic,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use haest_core::{
    ChildEnv,
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, OutputChunks},
};
use tokio::process::Command;

use super::pipe::{Listener, Sink, drain, take};

/// After a kill, how long the pipes may stay open (a grandchild that left the
/// group still holds them) before the result is returned without them.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// The group a child of `pid` leads, if `pid` can be one: never 0 (kill(0)
/// is the caller's own group) or 1 (kill(-1) is every process the caller may
/// signal, and init leads no child's group).
fn group_of(pid: u32) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|p| *p > 1)
}

/// Kill a detached child's whole process group (the child, and everything it
/// spawned) with SIGKILL.
///
/// `pid` is the id of a child that leads its own group, as
/// [`RealWorkspace`](crate::RealWorkspace) starts every child; when the group
/// is gone already, the process alone is signalled. A pid of 0 or 1 is refused
/// and nothing is signalled: `kill(0)` is the caller's own group, `kill(-1)`
/// every process the caller may signal. Errors are ignored: a process that
/// has exited is the goal.
#[expect(unsafe_code, reason = "kill(2) signals a process group; std cannot")]
pub(crate) fn kill_group(pid: u32) {
    let Some(pid) = group_of(pid) else {
        return;
    };
    // SAFETY: kill(2) takes plain integers and touches no memory of ours.
    let group_gone = unsafe { libc::kill(-pid, libc::SIGKILL) } != 0;
    if group_gone {
        // Only an unreaped child can still be here (its callers wait on it
        // last), so this reaches it and nothing else.
        // SAFETY: the same call on the process alone: plain integers, no memory.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Where a bare name is looked up when the host hands in no `PATH`: the
/// system's default, as `execvp` does. The child still gets no `PATH`.
const DEFAULT_PATH: &str = "/usr/bin:/bin";

/// Where `program` would run from: a path with a slash is taken from `cwd`,
/// a bare name is looked up on `env`'s `PATH`, else on [`DEFAULT_PATH`].
pub(super) fn which(program: &str, cwd: &Path, env: &ChildEnv) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let p = cwd.join(program);
        return is_executable(&p).then_some(p);
    }
    let path = env.get("PATH").unwrap_or(DEFAULT_PATH);
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The shell's exit code for a status: the code, or 128 + the signal that ended it.
fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(-1)
}

fn not_started(program: &str, e: &io::Error) -> ExecResult {
    let exit_code = match e.kind() {
        io::ErrorKind::NotFound => EXIT_NOT_FOUND,
        _ => 126,
    };
    ExecResult {
        stderr: format!("{program}: {e}"),
        exit_code,
        ..Default::default()
    }
}

/// Kills the child's whole group if the run is dropped before it ends (a
/// Code run that times out drops its calls in flight): `kill_on_drop` alone
/// kills only bash, and `sleep 1000 &` or `npm run dev` would run on.
struct KillGroupOnDrop(Option<u32>);

impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_group(pid);
        }
    }
}

/// Spawn `program args...` in its own process group, with `env` and nothing
/// else, and collect its output (or hand it to `stream` as it arrives: then the
/// result carries none). The deadline and the cancel token both kill the group.
/// A panic in `stream` goes on once the call has cleaned up, as the trait's
/// default raises it once the command has ended.
pub(super) async fn spawn_collect(
    program: &Path,
    args: &[String],
    opts: ExecOptions,
    stream: Option<OutputChunks>,
    env: &ChildEnv,
) -> ExecResult {
    let shown = program.display().to_string();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&opts.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(env.vars().iter().map(|(k, v)| (k, v)))
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return not_started(&shown, &e),
    };
    let pid = child.id().unwrap_or(0);
    let mut on_drop = KillGroupOnDrop(Some(pid));
    let (out, err): (Sink, Sink) = Default::default();
    let listener = stream.map(|s| Arc::new(Listener::new(s)));
    // Held in a set, never detached: a dropped call drops the set, which
    // aborts them.
    let mut readers = tokio::task::JoinSet::new();
    readers.spawn(drain(
        child.stdout.take().expect("piped"),
        out.clone(),
        listener.clone(),
    ));
    readers.spawn(drain(
        child.stderr.take().expect("piped"),
        err.clone(),
        listener.clone(),
    ));

    let mut timed_out = false;
    let status = {
        // Done = both pipes closed (a background job may hold them) AND the
        // child exited. Waited on LAST: an exited child stays a zombie until
        // then, so its pid and its group id stay ours for the kills below (a
        // reaped pid may already be another process's).
        let finished = async {
            while readers.join_next().await.is_some() {}
            child.wait().await
        };
        tokio::pin!(finished);
        tokio::select! {
            s = &mut finished => s.ok(),
            _ = tokio::time::sleep(opts.timeout) => {
                timed_out = true;
                kill_group(pid);
                tokio::time::timeout(DRAIN_GRACE, &mut finished).await.ok().and_then(Result::ok)
            }
            _ = opts.cancel.cancelled() => {
                kill_group(pid);
                tokio::time::timeout(DRAIN_GRACE, &mut finished).await.ok().and_then(Result::ok)
            }
        }
    };
    // It ended, or was killed above: the run was not dropped. Before the
    // await below: a reaped child's pid is no longer ours to kill.
    on_drop.0 = None;
    // The pipes are this call's no longer. A process that left the group may
    // hold them for good: stop reading, so nothing reaches `stream` after the
    // return and nothing more is kept. It gets SIGPIPE if it writes again.
    readers.shutdown().await;
    // The child is reaped and nothing reads for it: the caller's own panic
    // in `stream` goes on now.
    if let Some(panic) = listener.and_then(|l| l.take_panic()) {
        panic::resume_unwind(panic);
    }
    ExecResult {
        stdout: take(&out),
        stderr: take(&err),
        exit_code: status.map_or(128 + libc::SIGKILL, exit_code),
        timed_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_group_is_the_callers_own_or_every_process() {
        // Never signalled for real: kill(-1) reaches everything the user owns.
        assert_eq!(group_of(0), None);
        assert_eq!(group_of(1), None);
        assert_eq!(group_of(u32::MAX), None);
        assert_eq!(group_of(4242), Some(4242));
    }

    /// kill_group(0) would be kill(0): SIGKILL to the caller's own process group.
    /// Tried in a copy of this test binary that leads a group of its own, so
    /// nothing else is in reach. (1, kill(-1), is never tried: it would reach
    /// every process the user owns; the test above checks the refusal without a signal.)
    #[test]
    fn kill_group_never_signals_the_callers_own_group() {
        if std::env::var_os("HAEST_TEST_KILL_OWN_GROUP").is_some() {
            kill_group(0);
            return;
        }
        use std::os::unix::process::CommandExt;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "real::exec::tests::kill_group_never_signals_the_callers_own_group",
                "--exact",
            ])
            .env("HAEST_TEST_KILL_OWN_GROUP", "1")
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "the copy was killed: {status:?}");
    }
}
