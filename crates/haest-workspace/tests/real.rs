//! The real workspace: the host's bash and disk, killed as a whole process group.

mod common;

use std::{
    path::Path,
    time::{Duration, Instant},
};

use common::{argv, opts, real, tempdir};
use haest_core::{
    ChildEnv,
    fs::{FileKind, Fs, read_text_if_present},
    workspace::{EXIT_NOT_FOUND, Workspace, WorkspaceMode},
};
use haest_workspace::{RealFs, RealWorkspace};

#[tokio::test]
async fn a_child_sees_only_the_env_the_host_handed_in() {
    let (_t, dir) = tempdir();
    let path = std::env::var("PATH").unwrap();
    let env = ChildEnv::new([("PATH", path.as_str()), ("ONLY", "1")]);
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

/// A directory that does not exist failed the spawn with the OS's ENOENT,
/// reported as the program's: "/bin/bash: No such file or directory", when
/// bash was there all along.
#[tokio::test]
async fn a_command_in_a_missing_directory_says_the_directory_is_missing() {
    let (_t, dir) = tempdir();
    let gone = dir.join("gone");
    let r = real().shell("echo hi", opts(&gone)).await;
    assert_eq!(r.exit_code, EXIT_NOT_FOUND);
    assert!(
        r.stderr.contains(&gone.display().to_string()) && !r.stderr.contains("bash"),
        "{}",
        r.stderr
    );
}

/// A command's output was kept whole, so one that printed without end (`yes`,
/// `cat /dev/urandom`) filled the host's memory until its timeout. Each
/// stream keeps its first 64 MiB, as the sandbox does, and the child is
/// still read to its end: it never blocks on a full pipe.
#[tokio::test]
async fn each_stream_keeps_its_first_64_mib_and_the_child_runs_to_its_end() {
    const KEPT: usize = 64 * 1024 * 1024;
    let (_t, dir) = tempdir();
    let past = KEPT + 1024 * 1024;
    let script = format!(
        "head -c {past} /dev/zero; head -c {past} /dev/zero >&2; echo then; echo exit-7 >&2; exit 7"
    );
    let r = real().shell(&script, opts(&dir)).await;
    assert_eq!((r.exit_code, r.timed_out), (7, false));
    assert_eq!(r.stdout.len(), KEPT, "stdout kept past the cap");
    assert_eq!(r.stderr.len(), KEPT, "stderr kept past the cap");
    assert!(r.stdout.bytes().all(|b| b == 0));
}

#[tokio::test]
async fn output_that_is_not_utf8_is_decoded_lossily() {
    let (_t, dir) = tempdir();
    let r = real().shell(r"printf 'ok\377'", opts(&dir)).await;
    assert_eq!(r.stdout, "ok\u{FFFD}");
}

#[expect(
    unsafe_code,
    reason = "kill(2) with signal 0 asks whether a pid lives; std cannot"
)]
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

/// bash is reaped last: while a process that left its group holds the call's
/// output, an exited bash stays a zombie, so its pid and group id stay this
/// call's for the timeout's kill. Reaped, both are free for the system to
/// hand to an unrelated process, which the kill would then reach.
#[tokio::test(flavor = "multi_thread")]
async fn an_exited_bash_keeps_its_pid_until_the_call_ends() {
    let (_t, dir) = tempdir();
    let mut o = opts(&dir);
    o.timeout = Duration::from_secs(2);
    let ws = real();
    let detached = "perl -e 'use POSIX qw(setsid); setsid(); sleep 3'";
    let call = tokio::spawn(async move {
        ws.shell(&format!("echo $$ > bash.pid; {detached} & sleep 0.2"), o)
            .await
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let pid = std::fs::read_to_string(dir.join("bash.pid")).unwrap();
    let ps = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&ps.stdout).trim().to_string();

    assert!(
        stat.starts_with('Z'),
        "bash ({}) reads {stat:?}",
        pid.trim()
    );
    assert!(call.await.unwrap().timed_out);
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    unsafe_code,
    reason = "kill(2) cleans up a child that survived; std cannot"
)]
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

/// A hook's payload goes on its stdin: written whole, then closed, so a
/// command that reads to the end (`cat`, `read`, `json.load(sys.stdin)`)
/// sees the end of it.
#[tokio::test]
async fn shell_with_stdin_feeds_the_command_and_closes_its_input() {
    let (_t, dir) = tempdir();
    let r = real()
        .shell_with_stdin("wc -c; echo done", "{\"event\":\"x\"}", opts(&dir))
        .await;
    assert_eq!(r.exit_code, 0, "{}", r.stderr);
    assert_eq!(
        r.stdout.split_whitespace().collect::<Vec<_>>(),
        ["13", "done"]
    );
}

/// A command that never reads its input, given more than a pipe holds,
/// still ends when it does: the write is refused, not waited on.
#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_ignores_a_large_stdin_still_ends() {
    let (_t, dir) = tempdir();
    let big = vec![b'x'; 4 * 1024 * 1024];
    let started = Instant::now();
    let r = real()
        .shell_with_stdin("echo ignored", big, opts(&dir))
        .await;
    assert_eq!((r.stdout.as_str(), r.exit_code), ("ignored\n", 0));
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The timeout and the cancel token kill a command that holds its input
/// open, as they kill any other.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_kills_a_command_reading_its_stdin() {
    let (_t, dir) = tempdir();
    let o = opts(&dir);
    let cancel = o.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let r = real()
        .shell_with_stdin("cat >/dev/null; sleep 30", "in", o)
        .await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_ne!(r.exit_code, 0);
}

#[tokio::test]
async fn real_fs_reads_writes_and_lists_the_disk() {
    let (_t, dir) = tempdir();
    let fs = RealFs;
    let p = |rel: &str| dir.join(rel);
    fs.create_dir_all(&p("sub/deep")).await.unwrap();
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
    assert!(fs.remove(&p("moved")).await.is_err());
    fs.remove_all(&p("moved")).await.unwrap();
    assert!(!Path::new(&p("moved")).exists());
}

#[tokio::test]
async fn a_config_under_a_file_is_absent_but_one_that_cannot_be_read_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = tempdir();
    let fs = RealFs;
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
    let fs = RealFs;
    let f = dir.join("session.jsonl");
    fs.write(&f, b"{}").await.unwrap();
    fs.chmod(&f, 0o600).await.unwrap();
    let mode = std::fs::metadata(&f).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
