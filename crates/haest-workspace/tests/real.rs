//! The real workspace: the host's bash and disk, killed as a whole process group.

mod common;

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use common::{argv, opts, real, tempdir};
use haest_core::{
    ChildEnv,
    fs::{FileKind, Fs, read_text_if_present},
    workspace::{EXIT_NOT_FOUND, OutputChunks, Workspace, WorkspaceMode},
};
use haest_workspace::{RealFs, RealWorkspace};

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
        .run(&argv(&["haest-no-such-program-xyz"]), opts(&dir))
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
async fn a_dropped_run_kills_the_whole_process_group() {
    let (_t, dir) = tempdir();
    let pid_file = dir.join("child.pid");
    let started = async {
        loop {
            if let Ok(s) = std::fs::read_to_string(&pid_file)
                && s.ends_with('\n')
            {
                break s.trim().parse::<libc::pid_t>().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let ws = real();
    // A Code run that times out drops its calls mid-way, just so.
    let pid = tokio::select! {
        _ = ws.shell("sleep 30 & echo $! > child.pid; wait", opts(&dir)) => {
            panic!("the shell ended on its own")
        }
        pid = started => pid,
    };
    let gone = Instant::now() + Duration::from_secs(2);
    while alive(pid) && Instant::now() < gone {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let survived = alive(pid);
    if survived {
        // SAFETY: our own test's sleep; kill(2) touches no memory of ours.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(!survived, "the background child outlived the dropped run");
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

/// kill_group(0) would be kill(0): SIGKILL to the caller's own process group.
/// Tried in a copy of this test binary that leads a group of its own, so
/// nothing else is in reach. (1, kill(-1), is never tried: it would reach
/// every process the user owns; exec.rs tests the refusal without a signal.)
#[test]
fn kill_group_never_signals_the_callers_own_group() {
    if std::env::var_os("HAEST_TEST_KILL_OWN_GROUP").is_some() {
        haest_workspace::kill_group(0);
        return;
    }
    use std::os::unix::process::CommandExt;
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["kill_group_never_signals_the_callers_own_group", "--exact"])
        .env("HAEST_TEST_KILL_OWN_GROUP", "1")
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the copy was killed: {status:?}");
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
async fn a_config_under_a_file_is_absent_but_one_that_cannot_be_read_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = tempdir();
    let fs = RealFs::new();
    std::fs::write(dir.join("plain"), "a file where a dir is expected").unwrap();
    let under_a_file = read_text_if_present(&fs, &dir.join("plain/config.toml")).await;
    assert_eq!(under_a_file.unwrap(), None, "no such file can exist there");

    let locked = dir.join("locked.toml");
    std::fs::write(&locked, "model = \"x\"").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = read_text_if_present(&fs, &locked).await;
    assert!(
        unreadable.is_err(),
        "a file that is there is never a silent default"
    );
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
