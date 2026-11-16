//! The sandbox: a simulated bash and the tools share ONE file system, and the
//! project on disk is never written before /apply.

mod common;

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use common::{argv, opts, sandbox, tempdir};
use kern_core::{
    fs::{Fs, MemFs},
    workspace::{EXIT_NOT_FOUND, Workspace, WorkspaceMode},
};
use kern_workspace::{SandboxMounts, SandboxWorkspace};

#[tokio::test]
async fn a_file_the_tools_write_is_what_cat_prints_and_the_other_way_round() {
    let (_t, dir) = tempdir();
    std::fs::write(dir.join("disk.txt"), "from disk\n").unwrap();
    let ws = sandbox(&dir).await;
    assert_eq!(ws.mode(), WorkspaceMode::Sandbox);
    let fs = ws.fs();

    fs.write(&dir.join("tool.txt"), b"from the tool\n")
        .await
        .unwrap();
    let r = ws.shell("cat tool.txt disk.txt", opts(&dir)).await;
    assert_eq!(r.stdout, "from the tool\nfrom disk\n");

    ws.shell(
        "mkdir -p out && echo 'from the shell' > out/sh.txt",
        opts(&dir),
    )
    .await;
    assert_eq!(
        fs.read_to_string(&dir.join("out/sh.txt")).await.unwrap(),
        "from the shell\n"
    );
    let names: Vec<_> = fs
        .read_dir(&dir)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["disk.txt", "out", "tool.txt"]);
}

#[tokio::test]
async fn the_project_listing_is_the_disks_with_nothing_seeded_into_it() {
    let (_t, dir) = tempdir();
    std::fs::create_dir(dir.join("tmp")).unwrap();
    std::fs::write(dir.join("tmp/x.txt"), "mine\n").unwrap();
    let ws = sandbox(&dir).await;
    let r = ws.shell("ls; cat tmp/x.txt", opts(&dir)).await;
    assert_eq!(r.stdout, "tmp\nmine\n", "{r:?}");
}

#[tokio::test]
async fn writes_never_reach_the_real_disk() {
    let (_t, dir) = tempdir();
    std::fs::write(dir.join("keep.txt"), "keep\n").unwrap();
    let ws = sandbox(&dir).await;
    ws.fs().write(&dir.join("ghost.txt"), b"x").await.unwrap();
    ws.shell("echo y > ghost2.txt; rm -f keep.txt nothing", opts(&dir))
        .await;
    assert!(!dir.join("ghost.txt").exists());
    assert!(!dir.join("ghost2.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert!(!ws.fs().exists(&dir.join("keep.txt")).await);
}

#[tokio::test]
async fn the_host_environment_does_not_leak_into_the_shell() {
    let (_t, dir) = tempdir();
    // SAFETY: a variable no other test reads or writes.
    unsafe { std::env::set_var("KERN_WORKSPACE_TEST_SECRET", "sk-leak-me") };
    let r = sandbox(&dir)
        .await
        .shell("env; echo \"[$KERN_WORKSPACE_TEST_SECRET]\"", opts(&dir))
        .await;
    assert!(!r.stdout.contains("sk-leak-me"));
    assert!(r.stdout.contains("[]"));
}

#[tokio::test]
async fn no_real_programs_and_no_network() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    for cmd in [
        "git status",
        "python3 -c 1",
        "node -e 1",
        "curl -s https://example.com",
    ] {
        let r = ws.shell(cmd, opts(&dir)).await;
        assert_ne!(r.exit_code, 0, "{cmd} ran: {r:?}");
    }
}

#[tokio::test]
async fn run_passes_argv_untouched_and_a_missing_program_is_127() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    let r = ws
        .run(&argv(&["echo", "a  b", "$HOME", "*"]), opts(&dir))
        .await;
    assert_eq!(r.stdout, "a  b $HOME *\n");
    let missing = ws.run(&argv(&["no-such-program-xyz"]), opts(&dir)).await;
    assert_eq!(missing.exit_code, EXIT_NOT_FOUND);
    assert_eq!(ws.run(&[], opts(&dir)).await.exit_code, EXIT_NOT_FOUND);
}

#[tokio::test]
async fn each_call_is_a_fresh_shell_over_the_same_files() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    ws.shell("X=1; mkdir sub; cd sub; echo kept > f", opts(&dir))
        .await;
    let r = ws.shell("echo \"[$X]\"; pwd; cat sub/f", opts(&dir)).await;
    assert_eq!(r.stdout, format!("[]\n{}\nkept\n", dir.display()), "{r:?}");
}

#[tokio::test]
async fn a_writable_mount_goes_through_to_the_disk_and_a_readable_one_is_read_only() {
    let (_t, dir) = tempdir();
    let (_o, out) = tempdir();
    let (_s, skills) = tempdir();
    std::fs::write(skills.join("SKILL.md"), "skill\n").unwrap();
    let ws = SandboxWorkspace::open(SandboxMounts::new(&dir).writable(&out).readable(&skills))
        .await
        .unwrap();

    ws.fs().write(&out.join("kept.txt"), b"kept").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("kept.txt")).unwrap(),
        "kept"
    );

    let read = ws
        .shell(&format!("cat {}/SKILL.md", skills.display()), opts(&dir))
        .await;
    assert_eq!(read.stdout, "skill\n");
    let write = ws
        .shell(&format!("echo x > {}/new.md", skills.display()), opts(&dir))
        .await;
    assert_ne!(write.exit_code, 0);
    assert!(!skills.join("new.md").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_a_long_sleep_quickly() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    let o = opts(&dir);
    let cancel = o.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let r = ws.shell("echo started; sleep 30", o).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(!r.timed_out);
    assert_ne!(r.exit_code, 0);
    assert_eq!(r.stdout, "started\n", "output before the cancel is kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_timeout_stops_the_call_and_says_so() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    let mut o = opts(&dir);
    o.timeout = Duration::from_millis(300);
    let started = Instant::now();
    let r = ws.shell("sleep 30", o).await;
    assert!(r.timed_out);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn a_sandbox_over_a_hosts_own_fs_shares_its_files_with_the_shell() {
    let mem: Arc<dyn Fs> = Arc::new(MemFs::with_files([("/w/seed.txt", "seed\n")]));
    let ws = SandboxWorkspace::over(mem.clone());
    assert!(ws.changes().is_none());
    let o = opts(Path::new("/w"));

    let r = ws.shell("echo hi > f && cat f", o.clone()).await;
    assert_eq!(r.stdout, "hi\n", "{r:?}");
    assert_eq!(mem.read_to_string(Path::new("/w/f")).await.unwrap(), "hi\n");

    mem.write(Path::new("/w/tool.txt"), b"tool\n")
        .await
        .unwrap();
    let r = ws
        .shell(
            "cat seed.txt tool.txt; mkdir -p a/b && mv f a/b/g && ls a/b",
            o.clone(),
        )
        .await;
    assert_eq!(r.stdout, "seed\ntool\ng\n", "{r:?}");
    assert!(mem.exists(Path::new("/w/a/b/g")).await);

    let r = ws
        .shell("printf 'echo ran' > s.sh && chmod +x s.sh && ./s.sh", o)
        .await;
    assert_eq!(r.stdout, "ran\n", "{r:?}");
}

#[tokio::test]
async fn a_host_may_replace_the_prompt_note() {
    let ws = SandboxWorkspace::over(Arc::new(MemFs::new()));
    assert!(!ws.prompt_note().is_empty());
    let ws = ws.with_prompt_note("custom");
    assert_eq!(ws.prompt_note(), "custom");
}
