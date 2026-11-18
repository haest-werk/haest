//! A file's mode in the sandbox, and on the disk once applied: what the
//! sandbox set with `chmod` is listed and applied; applying takes any
//! permission away but grants none but execute, and that only where the
//! file can be read.

mod common;

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use common::{
    opts, sandbox,
    spelling::{apply_all, changes, paths, run},
    tempdir,
};
use haest_core::workspace::{ApplyFailureCode, ChangeKind, Workspace};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Executable for each class that may read it, as `chmod +x` makes a file
/// under the disk's own umask.
fn executable(path: &Path) -> bool {
    let m = mode(path);
    m & 0o100 != 0 && m & 0o111 == (m & 0o444) >> 2
}

/// The sandbox ran its `chmod +x`, and `./run.sh` worked there. Neither
/// mode was listed or applied: the new script landed without `+x`, and
/// `lib/a.rs` was not listed at all.
#[tokio::test]
async fn a_mode_the_sandbox_sets_is_listed_and_applied() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    set_mode(&dir.join("lib/a.rs"), 0o644);
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "printf 'echo hi\\n' > run.sh && chmod +x run.sh && chmod +x lib/a.rs",
    )
    .await;
    let mut listed: Vec<_> = changes(&ws)
        .list()
        .await
        .into_iter()
        .map(|c| (c.path, c.kind))
        .collect();
    listed.sort_by(|a, b| a.0.cmp(&b.0));
    let want = [
        ("/lib/a.rs".to_owned(), ChangeKind::Modified),
        ("/run.sh".to_owned(), ChangeKind::Added),
    ];
    assert_eq!(listed, want);
    apply_all(&ws).await;
    assert!(
        executable(&dir.join("run.sh")),
        "{:o}",
        mode(&dir.join("run.sh"))
    );
    assert!(
        executable(&dir.join("lib/a.rs")),
        "{:o}",
        mode(&dir.join("lib/a.rs"))
    );
    assert_eq!(fs::read_to_string(dir.join("lib/a.rs")).unwrap(), "a\n");
}

/// A mode change with nothing else is shown for review, not an empty diff.
#[tokio::test]
async fn a_mode_change_alone_shows_in_its_diff() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("run.sh"), "echo hi\n").unwrap();
    set_mode(&dir.join("run.sh"), 0o644);
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "chmod +x run.sh").await;
    let list = changes(&ws).list().await;
    assert_eq!(list.len(), 1, "{list:?}");
    let diff = changes(&ws).diff(&list[0]).await;
    assert_eq!(diff, "Mode /run.sh 644 -> 755\n", "{list:?}");
}

/// What `chmod` takes away is applied: `chmod -x` and `chmod 600`.
#[tokio::test]
async fn a_mode_taken_away_is_applied() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("tool.sh"), "echo hi\n").unwrap();
    set_mode(&dir.join("tool.sh"), 0o755);
    fs::write(dir.join("secret.env"), "KEY=1\n").unwrap();
    set_mode(&dir.join("secret.env"), 0o644);
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "chmod -x tool.sh && chmod 600 secret.env").await;
    apply_all(&ws).await;
    assert_eq!(mode(&dir.join("tool.sh")), 0o644);
    assert_eq!(mode(&dir.join("secret.env")), 0o600);
}

/// Applying never lets more people read or change a file than the disk
/// does: `chmod 666` keeps the disk's 644.
#[tokio::test]
async fn a_mode_never_grants_read_or_write_on_the_disk() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    set_mode(&dir.join("a.txt"), 0o644);
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "chmod 666 a.txt").await;
    let _ = changes(&ws).apply().await;
    assert_eq!(mode(&dir.join("a.txt")), 0o644);
}

/// A script the sandbox moves stays executable (a rename keeps the mode),
/// and one the sandbox makes executable, then moves, too.
#[tokio::test]
async fn a_moved_script_stays_executable() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("run.sh"), "echo hi\n").unwrap();
    set_mode(&dir.join("run.sh"), 0o755);
    fs::write(dir.join("new.sh"), "echo new\n").unwrap();
    set_mode(&dir.join("new.sh"), 0o644);
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "mkdir bin && mv run.sh bin/ && chmod +x new.sh && mv new.sh bin/new.sh",
    )
    .await;
    apply_all(&ws).await;
    for script in ["bin/run.sh", "bin/new.sh"] {
        let path = dir.join(script);
        assert!(executable(&path), "{script}: {:o}", mode(&path));
    }
}

/// An edit keeps the file's mode, in the sandbox as in bash: `>` and `>>`
/// made an executable file a plain one there.
#[tokio::test]
async fn an_edit_keeps_the_files_mode() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("run.sh"), "echo hi\n").unwrap();
    fs::write(dir.join("add.sh"), "echo hi\n").unwrap();
    set_mode(&dir.join("run.sh"), 0o755);
    set_mode(&dir.join("add.sh"), 0o755);
    let ws = sandbox(&dir).await;
    let edit = "echo 'echo x' > run.sh && echo 'echo y' >> add.sh \
                && test -x run.sh && test -x add.sh";
    let r = ws.shell(edit, opts(&dir)).await;
    assert_eq!(r.exit_code, 0, "{r:?}");
    apply_all(&ws).await;
    assert_eq!(mode(&dir.join("run.sh")), 0o755);
    assert_eq!(mode(&dir.join("add.sh")), 0o755);
}

/// A file the sandbox made executable, removed and made again is a new
/// file: the mode it had is gone with it.
#[tokio::test]
async fn a_file_made_again_does_not_keep_the_old_ones_mode() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.sh"), "echo a\n").unwrap();
    set_mode(&dir.join("a.sh"), 0o644);
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "chmod +x a.sh && rm a.sh && echo b > a.sh").await;
    apply_all(&ws).await;
    assert_eq!(mode(&dir.join("a.sh")), 0o644);
}

/// Bash writes into a directory, then makes it read-only. Apply put each
/// directory's mode on first (path order), so the write into it then failed
/// with "Permission denied".
#[tokio::test]
async fn a_directory_made_read_only_after_a_write_into_it_still_gets_the_write() {
    for (command, file, dirs) in [
        ("echo x > out/g && chmod 555 out", "out/g", &["out"][..]),
        (
            "mkdir -p gen/sub && echo x > gen/sub/a && chmod 555 gen/sub gen",
            "gen/sub/a",
            &["gen/sub", "gen"][..],
        ),
    ] {
        let (_t, dir) = tempdir();
        fs::create_dir(dir.join("out")).unwrap();
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        let report = changes(&ws).apply().await;
        let modes: Vec<String> = dirs
            .iter()
            .map(|d| format!("{:o}", mode(&dir.join(d))))
            .collect();
        // Writable again, so the temp dir goes.
        for d in dirs.iter().rev() {
            set_mode(&dir.join(d), 0o755);
        }
        assert!(report.failed.is_empty(), "{command}: {report:?}");
        let landed = fs::read_to_string(dir.join(file)).ok();
        assert_eq!(landed.as_deref(), Some("x\n"), "{command}");
        assert!(modes.iter().all(|m| m == "555"), "{command}: {modes:?}");
    }
}

/// A directory's mode the disk refuses (a locked directory) fails alone,
/// once the rest is written, and stays listed.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_directory_mode_the_disk_refuses_fails_and_stays_listed() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("out")).unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "chmod 700 out && echo x > note.md").await;
    let lock = |flag: &str| {
        let status = std::process::Command::new("chflags")
            .args([flag, &dir.join("out").to_string_lossy()])
            .status();
        assert!(status.unwrap().success(), "chflags {flag}");
    };
    lock("uchg");
    let report = changes(&ws).apply().await;
    let left = changes(&ws).list().await;
    lock("nouchg");
    assert_eq!(paths(&report.applied), ["/note.md"], "{report:?}");
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|f| (f.change.path.as_str(), f.code))
        .collect();
    assert_eq!(failed, [("/out", ApplyFailureCode::WriteFailed)]);
    assert_eq!(paths(&left), ["/out"]);
}

/// The human changes a file's mode after the sandbox set its own: the
/// sandbox's is a conflict, never put over the human's.
#[tokio::test]
async fn a_mode_the_human_changes_meanwhile_is_a_conflict() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("run.sh"), "echo hi\n").unwrap();
    set_mode(&dir.join("run.sh"), 0o644);
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "chmod +x run.sh").await;
    set_mode(&dir.join("run.sh"), 0o600);
    let report = changes(&ws).apply().await;
    assert!(report.applied.is_empty(), "{report:?}");
    assert!(report.skipped.iter().all(|c| c.conflict), "{report:?}");
    assert_eq!(mode(&dir.join("run.sh")), 0o600);
}
