//! The real workspace: the host's bash and disk, killed as a whole process group.

mod common;

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use common::{argv, opts, real, tempdir};
use kern_core::{
    ChildEnv,
    fs::{FileKind, Fs},
    workspace::{EXIT_NOT_FOUND, OutputChunks, Workspace, WorkspaceMode},
};
use kern_workspace::{RealFs, RealWorkspace};

#[tokio::test]
async fn a_child_sees_only_the_env_the_host_handed_in() {
    let (_t, dir) = tempdir();
    let path = std::env::var("PATH").unwrap();
    let env = ChildEnv::new([("PATH".into(), path), ("ONLY".into(), "1".into())]);
    let r = RealWorkspace::new(env)
        .shell("echo \"${HOME-unset} $ONLY\"", opts(&dir))
        .await;
    assert_eq!(r.stdout, "unset 1\n");
}

#[tokio::test]
async fn with_nothing_handed_in_bash_still_runs_from_the_systems_default_path() {
    let (_t, dir) = tempdir();
    let bare = RealWorkspace::new(ChildEnv::default());
    let r = bare.shell("echo \"${HOME-unset}\"", opts(&dir)).await;
    assert_eq!(
        (r.stdout.as_str(), r.exit_code),
        ("unset\n", 0),
        "{}",
        r.stderr
    );
    let ls = bare.run(&argv(&["ls"]), opts(&dir)).await;
    assert_eq!(ls.exit_code, 0, "{}", ls.stderr);
}

#[tokio::test]
async fn shell_runs_bash_in_the_given_directory_and_reports_both_streams() {
    let (_t, dir) = tempdir();
    let ws = real();
    assert_eq!(ws.mode(), WorkspaceMode::Real);
    let r = ws.shell("pwd; echo oops >&2; exit 3", opts(&dir)).await;
    assert_eq!(r.stdout.trim(), dir.to_str().unwrap());
    assert_eq!(r.stderr, "oops\n");
    assert_eq!(r.exit_code, 3);
    assert!(!r.timed_out);
}

#[tokio::test]
async fn run_passes_argv_untouched_and_reports_a_missing_program_as_127() {
    let (_t, dir) = tempdir();
    let ws = real();
    let r = ws
        .run(&argv(&["echo", "a  b", "$HOME", "*"]), opts(&dir))
        .await;
    assert_eq!(r.stdout, "a  b $HOME *\n");
    let missing = ws
        .run(&argv(&["kern-no-such-program-xyz"]), opts(&dir))
        .await;
    assert_eq!(missing.exit_code, EXIT_NOT_FOUND);
    assert_eq!(ws.run(&[], opts(&dir)).await.exit_code, EXIT_NOT_FOUND);
}

#[tokio::test]
async fn output_that_is_not_utf8_is_decoded_lossily() {
    let (_t, dir) = tempdir();
    let r = real().shell(r"printf 'ok\377'", opts(&dir)).await;
    assert_eq!(r.stdout, "ok\u{FFFD}");
}

#[tokio::test(flavor = "multi_thread")]
async fn streamed_output_arrives_while_the_command_runs_and_stays_out_of_the_result() {
    let (_t, dir) = tempdir();
    let seen = Arc::new(Mutex::new(String::new()));
    let into = seen.clone();
    let on_output: OutputChunks = Arc::new(move |s: &str| into.lock().unwrap().push_str(s));
    let ws = real();
    // A multi-byte character split across two writes arrives whole.
    let run = ws.shell_streaming(
        r"printf 'first \xc3'; sleep 0.05; printf '\xa9\n'; sleep 0.4; echo second >&2",
        opts(&dir),
        on_output,
    );
    tokio::pin!(run);
    let early = tokio::select! {
        r = &mut run => panic!("ended before the check: {r:?}"),
        _ = tokio::time::sleep(Duration::from_millis(250)) => seen.lock().unwrap().clone(),
    };
    assert_eq!(early, "first \u{e9}\n");
    let r = run.await;
    assert_eq!(*seen.lock().unwrap(), "first \u{e9}\nsecond\n");
    assert_eq!(
        (r.stdout.as_str(), r.stderr.as_str(), r.exit_code),
        ("", "", 0)
    );
}

fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_timeout_kills_the_whole_process_group_not_just_bash() {
    let (_t, dir) = tempdir();
    let mut o = opts(&dir);
    o.timeout = Duration::from_millis(500);
    let started = Instant::now();
    let r = real()
        .shell("sleep 30 & echo $! > child.pid; wait", o)
        .await;
    assert!(r.timed_out);
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid: libc::pid_t = std::fs::read_to_string(dir.join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let gone = Instant::now() + Duration::from_secs(2);
    while alive(pid) && Instant::now() < gone {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!alive(pid), "the background child outlived the timeout");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_kills_a_long_sleep_quickly() {
    let (_t, dir) = tempdir();
    let o = opts(&dir);
    let cancel = o.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let r = real().shell("sleep 30", o).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(!r.timed_out);
    assert_ne!(r.exit_code, 0);
}

#[tokio::test]
async fn real_fs_reads_writes_and_lists_the_disk() {
    let (_t, dir) = tempdir();
    let fs = RealFs::new();
    let p = |rel: &str| dir.join(rel);
    fs.mkdir(&p("sub/deep"), true).await.unwrap();
    fs.write(&p("sub/b.txt"), b"one").await.unwrap();
    fs.append(&p("sub/b.txt"), b" two").await.unwrap();
    assert_eq!(fs.read_to_string(&p("sub/b.txt")).await.unwrap(), "one two");
    assert_eq!(std::fs::read_to_string(p("sub/b.txt")).unwrap(), "one two");
    let b = p("sub/b.txt");
    let head = |max| fs.read_head(&b, max);
    assert_eq!(
        (head(3).await.unwrap(), head(99).await.unwrap()),
        (b"one".to_vec(), b"one two".to_vec())
    );
    assert_eq!(fs.stat(&p("sub/b.txt")).await.unwrap().size, 7);
    assert!(fs.write(&p("nope/x"), b"x").await.is_err());

    std::os::unix::fs::symlink(p("sub/b.txt"), p("sub/a-link")).unwrap();
    let listing: Vec<_> = fs
        .read_dir(&p("sub"))
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.name, e.kind))
        .collect();
    assert_eq!(
        listing,
        [
            ("a-link".to_string(), FileKind::Symlink),
            ("b.txt".to_string(), FileKind::File),
            ("deep".to_string(), FileKind::Dir),
        ]
    );
    // stat follows the link to the file it names.
    assert!(fs.stat(&p("sub/a-link")).await.unwrap().is_file());

    fs.rename(&p("sub"), &p("moved")).await.unwrap();
    assert!(fs.exists(&p("moved/b.txt")).await);
    assert!(fs.remove(&p("moved"), false).await.is_err());
    fs.remove(&p("moved"), true).await.unwrap();
    assert!(!Path::new(&p("moved")).exists());
}

#[tokio::test]
async fn chmod_sets_the_mode_on_the_disk_so_a_session_file_stays_private() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = tempdir();
    let fs = RealFs::new();
    let f = dir.join("session.jsonl");
    fs.write(&f, b"{}").await.unwrap();
    fs.chmod(&f, 0o600).await.unwrap();
    let mode = std::fs::metadata(&f).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
