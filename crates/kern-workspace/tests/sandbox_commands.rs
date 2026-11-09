//! A host's own commands in the sandbox shell: they run inside the shell call
//! like any builtin, over the same files, and the model is told about them.

mod common;

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use common::{opts, tempdir};
use kern_core::{
    shell_command::{ShellCall, ShellCommand},
    workspace::{ExecResult, Workspace},
};
use kern_workspace::{SandboxMounts, SandboxWorkspace};

/// Writes its args and stdin to `seen.txt` through kern's fs, prints the stdin
/// upper-cased, and exits with the code in `$EXIT_WITH`.
#[derive(Default)]
struct Shout {
    deadline: Mutex<Option<Instant>>,
}

#[async_trait]
impl ShellCommand for Shout {
    fn name(&self) -> &str {
        "shout"
    }
    fn note(&self) -> Option<String> {
        Some("Use `shout` to shout.".into())
    }
    async fn run(&self, call: ShellCall) -> ExecResult {
        *self.deadline.lock().unwrap() = Some(call.ends_at);
        let stdin = call.stdin.unwrap_or_default();
        let seen = format!("{}|{}\n", call.args.join(","), stdin.trim());
        call.fs
            .write(&call.cwd.join("seen.txt"), seen.as_bytes())
            .await
            .unwrap();
        ExecResult {
            stdout: stdin.to_uppercase(),
            stderr: "shouted\n".into(),
            exit_code: call.env.get("EXIT_WITH").map_or(0, |c| c.parse().unwrap()),
            timed_out: false,
        }
    }
}

struct Quiet;

#[async_trait]
impl ShellCommand for Quiet {
    fn name(&self) -> &str {
        "shout"
    }
    async fn run(&self, _call: ShellCall) -> ExecResult {
        ExecResult::default()
    }
}

async fn sandbox(dir: &Path) -> SandboxWorkspace {
    SandboxWorkspace::open(SandboxMounts::new(dir))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_command_runs_inside_the_shell_call_pipes_and_files_included() {
    let (_t, dir) = tempdir();
    let shout = Arc::new(Shout::default());
    let ws = sandbox(&dir).await.with_command(shout.clone());
    let started = Instant::now();
    let r = ws
        .shell(
            "echo hi | shout a b | tr H J; cat seen.txt; export EXIT_WITH=3; shout || echo failed $?",
            opts(&dir),
        )
        .await;
    assert_eq!(r.stdout, "JI\na,b|hi\nfailed 3\n");
    assert_eq!(r.stderr, "shouted\nshouted\n");
    // Its file is in the overlay, never on disk.
    assert!(!dir.join("seen.txt").exists());
    // The deadline is the shell call's: its 10s timeout from the start.
    let deadline = shout.deadline.lock().unwrap().unwrap();
    let left = deadline.duration_since(started);
    assert!(left > Duration::from_secs(9) && left <= Duration::from_secs(11));
}

#[tokio::test]
async fn its_note_joins_the_prompt_and_a_later_command_by_the_same_name_replaces_it() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await.with_command(Arc::new(Shout::default()));
    let note = ws.prompt_note();
    assert!(note.contains("Use `shout` to shout."));
    // The overlay's /apply note still closes it.
    assert!(note.ends_with("with the real file system."));

    let ws = ws.with_command(Arc::new(Quiet));
    assert!(!ws.prompt_note().contains("shout"));
    let r = ws.shell("echo hi | shout", opts(&dir)).await;
    assert_eq!((r.stdout.as_str(), r.exit_code), ("", 0));
}
