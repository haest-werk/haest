//! The real workspace: the host's bash and disk, killed as a whole process group.

mod common;

use std::{
    path::Path,
    time::{Duration, Instant},
};

use common::{argv, opts, tempdir};
use kern_core::{
    fs::{FileKind, Fs},
    workspace::{EXIT_NOT_FOUND, Workspace, WorkspaceMode},
};
use kern_workspace::{RealFs, RealWorkspace};

#[tokio::test]
async fn shell_runs_bash_in_the_given_directory_and_reports_both_streams() {
    let (_t, dir) = tempdir();
    let ws = RealWorkspace::new();
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
    let ws = RealWorkspace::new();
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
    let r = RealWorkspace::new()
        .shell(r"printf 'ok\377'", opts(&dir))
        .await;
    assert_eq!(r.stdout, "ok\u{FFFD}");
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
    let r = RealWorkspace::new()
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
    let r = RealWorkspace::new().shell("sleep 30", o).await;
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
