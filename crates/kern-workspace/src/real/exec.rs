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

use kern_core::workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

/// After a kill, how long the pipes may stay open (a grandchild that left the
/// group still holds them) before the result is returned without them.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Kill a detached child's whole process group (the child, and everything it spawned).
pub fn kill_group(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: kill(2) takes plain integers and touches no memory of ours.
    unsafe {
        if libc::kill(-pid, libc::SIGKILL) != 0 {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Where `program` would run from: a path with a slash is taken from `cwd`,
/// a bare name is looked up on `PATH`. None = no such executable file.
pub fn which(program: &str, cwd: &Path) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let p = cwd.join(program);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
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

async fn drain(mut from: impl AsyncRead + Unpin, into: Sink) {
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match from.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => into.lock().unwrap().extend_from_slice(&chunk[..n]),
        }
    }
}

fn take(s: &Sink) -> String {
    String::from_utf8_lossy(&std::mem::take(&mut *s.lock().unwrap())).into_owned()
}

/// Spawn `program args...` in its own process group and collect its output.
/// The deadline and the cancel token both kill the group.
pub async fn spawn_collect(program: &Path, args: &[String], opts: ExecOptions) -> ExecResult {
    let shown = program.display().to_string();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&opts.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return not_started(&shown, &e),
    };
    let pid = child.id().unwrap_or(0);
    let (out, err): (Sink, Sink) = Default::default();
    let readers = [
        tokio::spawn(drain(child.stdout.take().expect("piped"), out.clone())),
        tokio::spawn(drain(child.stderr.take().expect("piped"), err.clone())),
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
    ExecResult {
        stdout: take(&out),
        stderr: take(&err),
        exit_code: status.map_or(128 + libc::SIGKILL, exit_code),
        timed_out,
    }
}
