//! A host's own commands in the sandbox shell: they run inside the shell call
//! like any builtin, over the same files, and the model is told about them.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use common::{opts, sandbox, tempdir};
use haest_core::{
    ChildEnv,
    shell_command::{ShellCall, ShellCommand, ShellCommandRef},
    workspace::{ExecResult, Workspace},
};
use haest_workspace::SandboxWorkspace;

/// Writes its args and stdin to `seen.txt` through haest's fs, prints the stdin
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
        let stdin = call.stdin_text().unwrap_or_default().into_owned();
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

/// A call stops at the shell's max whatever its timeout
/// ([`MAX_SHELL_TIMEOUT`](haest_workspace::MAX_SHELL_TIMEOUT), or
/// `with_max_timeout`'s), so a command's deadline is that at most: never a
/// sum past what an `Instant` holds.
#[tokio::test]
async fn a_commands_deadline_is_the_shells_max_at_most() {
    let (_t, dir) = tempdir();
    for (max, secs) in [(None, 600), (Some(Duration::from_secs(1_200)), 1_200)] {
        let shout = Arc::new(Shout::default());
        let ws = sandbox(&dir).await;
        let ws = match max {
            Some(max) => ws.with_max_timeout(max),
            None => ws,
        };
        let ws = ws.with_command(shout.clone());
        let mut o = opts(&dir);
        o.timeout = Duration::MAX;
        let started = Instant::now();
        let r = ws.shell("echo hi | shout", o).await;
        assert_eq!((r.stdout.as_str(), r.exit_code), ("HI\n", 0), "{r:?}");
        let deadline = shout.deadline.lock().unwrap().unwrap();
        let left = deadline.duration_since(started);
        let (low, high) = (Duration::from_secs(secs - 1), Duration::from_secs(secs + 1));
        assert!(left > low && left <= high, "{max:?}: {left:?}");
    }
}

#[tokio::test]
async fn its_note_joins_the_prompt_and_a_later_command_by_the_same_name_replaces_it() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await.with_command(Arc::new(Shout::default()));
    let note = ws.prompt_note();
    assert!(note.contains("Use `shout` to shout."));
    // The overlay's part still closes it.
    let plain = sandbox(&dir).await.prompt_note();
    let (_, last) = plain.rsplit_once(". ").unwrap();
    assert!(note.ends_with(last));

    let ws = ws.with_command(Arc::new(Quiet));
    assert!(!ws.prompt_note().contains("shout"));
    let r = ws.shell("echo hi | shout", opts(&dir)).await;
    assert_eq!((r.stdout.as_str(), r.exit_code), ("", 0));
}

/// A list is added in order, each one as `with_command` adds it: the later
/// of two by one name wins.
#[tokio::test]
async fn commands_added_as_a_list_go_in_in_order() {
    let (_t, dir) = tempdir();
    let loud: ShellCommandRef = Arc::new(Shout::default()).into();
    let ws = sandbox(&dir)
        .await
        .with_commands([loud.clone(), ShellCommandRef::new(Quiet)]);
    assert!(!ws.prompt_note().contains("shout"));
    let ws = sandbox(&dir)
        .await
        .with_commands(vec![ShellCommandRef::new(Quiet), loud]);
    assert!(ws.prompt_note().contains("Use `shout` to shout."));
    let r = ws.shell("echo hi | shout", opts(&dir)).await;
    assert_eq!(r.stdout, "HI\n");
}

/// Keeps the bytes a pipe fed it.
#[derive(Default)]
struct Taste {
    fed: Mutex<Option<Vec<u8>>>,
}

#[async_trait]
impl ShellCommand for Taste {
    fn name(&self) -> &str {
        "taste"
    }
    async fn run(&self, call: ShellCall) -> ExecResult {
        *self.fed.lock().unwrap() = call.stdin;
        ExecResult::default()
    }
}

#[tokio::test]
async fn a_command_gets_what_a_pipe_feeds_it_byte_for_byte() {
    // A document piped into a converter is binary: not UTF-8, NULs inside.
    let bytes: &[u8] = &[0x50, 0x4b, 0x03, 0x04, 0xff, 0x00, 0xc3, 0x28, 0x80];
    let (_t, dir) = tempdir();
    std::fs::write(dir.join("book.xlsx"), bytes).unwrap();
    let taste = Arc::new(Taste::default());
    let ws = sandbox(&dir).await.with_command(taste.clone());
    let r = ws.shell("cat book.xlsx | taste", opts(&dir)).await;
    assert_eq!(r.exit_code, 0, "{r:?}");
    assert_eq!(taste.fed.lock().unwrap().as_deref(), Some(bytes));
    ws.shell("taste", opts(&dir)).await;
    assert_eq!(*taste.fed.lock().unwrap(), None);
}

#[tokio::test]
async fn every_sandbox_has_xan_over_its_own_files_and_pipes() {
    let (_t, dir) = tempdir();
    std::fs::write(
        dir.join("sales.csv"),
        "region,amount\nnorth,120\nsouth,80\nnorth,30\n",
    )
    .unwrap();
    let ws = sandbox(&dir).await;
    assert!(ws.prompt_note().contains("xan does CSV"));
    let r = ws
        .shell(
            "xan groupby region 'sum(amount) as total' sales.csv | xan sort -s total -NR > /tmp/t.csv; \
             cat /tmp/t.csv; xan filter 'amount > 50' < sales.csv | xan count",
            opts(&dir),
        )
        .await;
    assert_eq!(
        (r.stdout.as_str(), r.stderr.as_str()),
        ("region,total\nnorth,150\nsouth,80\n2\n", "")
    );
    // A file the shell wrote (in the overlay, not on disk) is one xan reads.
    let r = ws
        .shell(
            "printf 'a\\n1\\n2\\n' > n.csv && xan agg 'sum(a)' n.csv",
            opts(&dir),
        )
        .await;
    assert_eq!(r.stdout, "sum(a)\n3\n");
    let r = ws
        .shell("xan filter 'nope > 1' sales.csv; echo $?", opts(&dir))
        .await;
    assert!(r.stderr.contains("no column `nope`"), "{r:?}");
    assert_eq!(r.stdout, "1\n");
}

/// A host's own `xan`.
struct OtherXan;

#[async_trait]
impl ShellCommand for OtherXan {
    fn name(&self) -> &str {
        "xan"
    }
    async fn run(&self, _call: ShellCall) -> ExecResult {
        ExecResult {
            stdout: "the host's\n".into(),
            ..Default::default()
        }
    }
}

#[tokio::test]
async fn a_hosts_xan_replaces_the_sandboxs() {
    let (_t, dir) = tempdir();
    let ws = SandboxWorkspace::over(Arc::new(haest_core::fs::MemFs::new()))
        .with_command(Arc::new(OtherXan));
    assert_eq!(
        ws.shell("xan count", opts(&dir)).await.stdout,
        "the host's\n"
    );
    assert!(!ws.prompt_note().contains("xan does CSV"));
}

#[tokio::test]
async fn the_hosts_variables_win_over_the_shells_own() {
    let (_t, dir) = tempdir();
    let env = ChildEnv::new(
        [("USER", "brain"), ("HOME", "/tmp")].map(|(k, v)| (k.to_string(), v.to_string())),
    );
    let ws = sandbox(&dir).await.with_env(env);
    let r = ws.shell("echo $USER $HOME", opts(&dir)).await;
    assert_eq!(r.stdout, "brain /tmp\n");
    let bare = sandbox(&dir).await;
    assert_eq!(bare.shell("echo $USER", opts(&dir)).await.stdout, "user\n");
}
