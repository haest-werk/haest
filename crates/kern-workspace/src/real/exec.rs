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

use kern_core::workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, OutputChunks};
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

/// Where a pipe's bytes go: kept for the result, or handed on as text as they arrive.
#[derive(Clone)]
pub enum Output {
    Collect,
    Stream(OutputChunks),
}

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

fn take(s: &Sink) -> String {
    String::from_utf8_lossy(&std::mem::take(&mut *s.lock().unwrap())).into_owned()
}

/// Spawn `program args...` in its own process group and collect its output
/// (or stream it: then the result carries none). The deadline and the cancel
/// token both kill the group.
pub async fn spawn_collect(
    program: &Path,
    args: &[String],
    opts: ExecOptions,
    output: Output,
) -> ExecResult {
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
    let stream = match output {
        Output::Collect => None,
        Output::Stream(on_output) => Some(on_output),
    };
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
    ExecResult {
        stdout: take(&out),
        stderr: take(&err),
        exit_code: status.map_or(128 + libc::SIGKILL, exit_code),
        timed_out,
    }
}
