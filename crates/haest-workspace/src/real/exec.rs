//! Running a real program. Children get their own process group so a timeout
//! or an interrupt kills the whole tree (`npm run dev` spawning node spawning
//! esbuild), not just bash.

use std::{
    io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_core::{
    ChildEnv,
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, OutputChunks},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

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
pub fn kill_group(pid: u32) {
    let Some(pid) = group_of(pid) else {
        return;
    };
    // SAFETY: kill(2) takes plain integers and touches no memory of ours.
    unsafe {
        if libc::kill(-pid, libc::SIGKILL) != 0 {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Where a bare name is looked up when the host hands in no `PATH`: the
/// system's default, as `execvp` does. The child still gets no `PATH`.
const DEFAULT_PATH: &str = "/usr/bin:/bin";

/// Where `program` would run from: a path with a slash is taken from `cwd`,
/// a bare name is looked up on `env`'s `PATH`, else on [`DEFAULT_PATH`].
pub fn which(program: &str, cwd: &Path, env: &ChildEnv) -> Option<PathBuf> {
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

type Sink = Arc<Mutex<Vec<u8>>>;

/// How many bytes at the end of `b` begin a character the next chunk completes.
fn cut_char(b: &[u8]) -> usize {
    for back in 1..=b.len().min(3) {
        let byte = b[b.len() - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let need = match byte {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        return if need > back { back } else { 0 };
    }
    0
}

/// The text at the front of `pending` that is whole characters (all of it at
/// the end of the stream); bytes that are not UTF-8 become U+FFFD.
fn whole_chars(pending: &mut Vec<u8>, end: bool) -> String {
    let upto = if end {
        pending.len()
    } else {
        pending.len() - cut_char(pending)
    };
    let text = String::from_utf8_lossy(&pending[..upto]).into_owned();
    pending.drain(..upto);
    text
}

async fn drain(mut from: impl AsyncRead + Unpin, into: Sink, stream: Option<OutputChunks>) {
    let mut chunk = vec![0u8; 16 * 1024];
    let mut pending = Vec::new();
    loop {
        let n = match from.read(&mut chunk).await {
            Ok(0) | Err(_) => 0,
            Ok(n) => n,
        };
        let Some(on_output) = &stream else {
            if n == 0 {
                return;
            }
            into.lock().unwrap().extend_from_slice(&chunk[..n]);
            continue;
        };
        pending.extend_from_slice(&chunk[..n]);
        let text = whole_chars(&mut pending, n == 0);
        if !text.is_empty() {
            on_output(&text);
        }
        if n == 0 {
            return;
        }
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

fn take(s: &Sink) -> String {
    String::from_utf8_lossy(&std::mem::take(&mut *s.lock().unwrap())).into_owned()
}

/// Spawn `program args...` in its own process group, with `env` and nothing
/// else, and collect its output (or hand it to `stream` as it arrives: then the
/// result carries none). The deadline and the cancel token both kill the group.
pub async fn spawn_collect(
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
    let readers = [
        tokio::spawn(drain(
            child.stdout.take().expect("piped"),
            out.clone(),
            stream.clone(),
        )),
        tokio::spawn(drain(
            child.stderr.take().expect("piped"),
            err.clone(),
            stream,
        )),
    ];
    // Done = the child exited AND both pipes closed (a background job may hold them).
    let finished = async {
        let status = child.wait().await;
        for r in readers {
            let _ = r.await;
        }
        status
    };
    tokio::pin!(finished);

    let mut timed_out = false;
    let status = tokio::select! {
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
    };
    // It ended, or was killed above: the run was not dropped.
    on_drop.0 = None;
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
}
