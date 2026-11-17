//! `js` in a real sandbox shell: registered through haest-core's ShellCommand,
//! it runs inside the shell call over the sandbox's own files.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use haest_code::sandbox_commands;
use haest_core::workspace::{ExecOptions, Workspace};
use haest_workspace::{SandboxMounts, SandboxWorkspace};

async fn sandbox(dir: &Path) -> SandboxWorkspace {
    let ws = SandboxWorkspace::open(SandboxMounts::new(dir))
        .await
        .unwrap();
    sandbox_commands()
        .into_iter()
        .fold(ws, SandboxWorkspace::with_command)
}

fn opts(cwd: &Path, timeout: Duration) -> ExecOptions {
    ExecOptions::new(cwd, timeout)
}

fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(t.path()).unwrap();
    (t, real)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_typescript_file_the_tools_wrote_runs_in_a_pipe_and_writes_back() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    let script = "import { writeFile } from 'fs/promises'
const who: string = process.argv[2]
await writeFile('out.txt', `hello ${who}`)
console.log(`hi ${who}`)";
    ws.fs()
        .write(&dir.join("hi.ts"), script.as_bytes())
        .await
        .unwrap();
    let r = ws
        .shell(
            "js hi.ts haest | tr a-z A-Z && cat out.txt",
            opts(&dir, Duration::from_secs(20)),
        )
        .await;
    assert_eq!(
        (r.stdout.as_str(), r.exit_code),
        ("HI HAEST\nhello haest", 0)
    );
    // The sandbox's overlay holds it; the disk does not.
    assert!(!dir.join("out.txt").exists());
}

#[tokio::test]
async fn node_points_at_js_and_the_prompt_says_js_is_there() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    let r = ws
        .shell("node x.js", opts(&dir, Duration::from_secs(10)))
        .await;
    assert_eq!(r.exit_code, 127);
    assert!(r.stderr.contains("js <file>"), "{}", r.stderr);
    assert!(ws.prompt_note().contains("`js <file.js|file.ts> [args]`"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_loop_stops_at_the_shell_calls_own_timeout() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    ws.fs()
        .write(&dir.join("spin.js"), b"while (true) {}")
        .await
        .unwrap();
    let started = Instant::now();
    let r = ws
        .shell("js spin.js", opts(&dir, Duration::from_secs(1)))
        .await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_ne!(r.exit_code, 0);
}

/// What is piped into `js` reaches the script, the ways a Node script reads
/// it: `process.stdin` iterated, its `data` and `end` events, `/dev/stdin`.
#[tokio::test(flavor = "multi_thread")]
async fn a_pipe_into_js_is_the_scripts_stdin() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    let script = "import { readFile } from 'fs/promises'
let it = ''
for await (const chunk of process.stdin) it += chunk
let ev = ''
await new Promise(done => process.stdin.on('data', c => { ev += c }).on('end', done))
const file = await readFile('/dev/stdin', 'utf8')
console.log([it, ev, file].map(t => t.trim().split('\\n').length).join(' '))";
    ws.fs()
        .write(&dir.join("lines.js"), script.as_bytes())
        .await
        .unwrap();
    let r = ws
        .shell(
            "printf 'a\\nb\\nc\\n' | js lines.js",
            opts(&dir, Duration::from_secs(20)),
        )
        .await;
    assert_eq!((r.stdout.trim(), r.exit_code), ("3 3 3", 0), "{}", r.stderr);
}
