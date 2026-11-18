//! A file's mode in the sandbox, and on the disk once applied: what the
//! sandbox set with `chmod` is listed and applied; applying takes any
//! permission away but grants none but execute, and that only where the
//! file can be read.

mod common;

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use common::{
    drop_apply_partway, opts, sandbox,
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

/// A path the sandbox adds with a mode of its own shows that mode in its
/// diff, as git's "new file mode" does: a script made executable and moved,
/// a 0600 key moved, a 0700 directory. The diff was content only, so the
/// mode that landed was never seen in review. A file made plainly shows
/// none.
#[tokio::test]
async fn an_added_paths_own_mode_shows_in_its_diff() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.sh"), "echo a\n").unwrap();
    set_mode(&dir.join("a.sh"), 0o644);
    fs::write(dir.join("secret.key"), "k\n").unwrap();
    set_mode(&dir.join("secret.key"), 0o600);
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "chmod +x a.sh && mv a.sh b.sh && mv secret.key moved.key \
         && mkdir private && chmod 700 private && echo n > new.txt",
    )
    .await;
    let set = changes(&ws);
    let mut firsts = Vec::new();
    for c in set.list().await {
        if c.kind == ChangeKind::Added {
            let diff = set.diff(&c).await;
            let first = diff.lines().next().unwrap_or_default().to_owned();
            firsts.push((c.path, first));
        }
    }
    let want = [
        ("/b.sh", "Mode /b.sh 755"),
        ("/moved.key", "Mode /moved.key 600"),
        ("/new.txt", "--- a/new.txt"),
        ("/private", "Mode /private 700"),
    ]
    .map(|(p, l)| (p.to_owned(), l.to_owned()));
    assert_eq!(firsts, want);
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

/// `mv` keeps a path's mode, and `cp` gives a new file its source's and
/// keeps the mode of a file it copies onto. A 0600 key and a 0700 directory
/// the sandbox moved or copied landed 0644 and 0755, readable by every
/// local account, and a key moved onto a file landed with that file's mode.
/// A file the sandbox made lands as made, moved or not.
#[tokio::test]
async fn a_path_the_sandbox_moves_or_copies_keeps_its_mode() {
    let (_t, dir) = tempdir();
    for (name, m) in [
        ("secret.key", 0o600),
        ("other.key", 0o600),
        ("config.key", 0o644),
        ("plain.txt", 0o644),
    ] {
        fs::write(dir.join(name), format!("{name}\n")).unwrap();
        set_mode(&dir.join(name), m);
    }
    fs::create_dir(dir.join("certs")).unwrap();
    fs::write(dir.join("certs/id"), "k\n").unwrap();
    set_mode(&dir.join("certs/id"), 0o600);
    set_mode(&dir.join("certs"), 0o700);
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "cp secret.key copy.key && cp secret.key plain.txt && cp -r certs copied \
         && mv secret.key moved.key && mv other.key config.key && mv certs keys \
         && echo n > new.txt && echo n > made.txt && mv made.txt fresh.txt",
    )
    .await;
    apply_all(&ws).await;
    let shown = |path: &str| format!("{:o}", mode(&dir.join(path)));
    let got: Vec<(&str, String)> = [
        "copy.key",
        "plain.txt",
        "copied",
        "copied/id",
        "moved.key",
        "config.key",
        "keys",
        "keys/id",
        "fresh.txt",
    ]
    .into_iter()
    .map(|p| (p, shown(p)))
    .collect();
    let mut want: Vec<(&str, String)> = [
        ("copy.key", "600"),
        ("plain.txt", "644"),
        ("copied", "700"),
        ("copied/id", "600"),
        ("moved.key", "600"),
        ("config.key", "600"),
        ("keys", "700"),
        ("keys/id", "600"),
    ]
    .into_iter()
    .map(|(p, m)| (p, m.to_owned()))
    .collect();
    want.push(("fresh.txt", shown("new.txt")));
    assert_eq!(got, want);
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

/// The haest command drops `/apply` 2 s after Ctrl+C. A directory's mode
/// goes on after every write, but the directory left the list as soon as
/// it was on the disk: a drop in between left the mode off the disk and
/// off the list, for good. The sandbox still showed it.
#[tokio::test]
async fn an_apply_dropped_partway_keeps_a_directorys_mode_listed() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("conf")).unwrap();
    set_mode(&dir.join("conf"), 0o755);
    fs::create_dir(dir.join("src")).unwrap();
    for i in 0..20 {
        fs::write(dir.join(format!("src/f{i:02}")), format!("{i}\n")).unwrap();
    }
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "chmod 700 conf && cp -r src gen && chmod 700 gen",
    )
    .await;
    let gen_dir = dir.join("gen");
    drop_apply_partway(&changes(&ws), || {
        fs::read_dir(&gen_dir).is_ok_and(|mut d| d.next().is_some())
    })
    .await;
    let listed = paths(&changes(&ws).list().await);
    for d in ["/conf", "/gen"] {
        assert!(listed.iter().any(|p| p == d), "{d} in {listed:?}");
    }
    apply_all(&ws).await;
    assert_eq!(mode(&dir.join("conf")), 0o700);
    assert_eq!(mode(&gen_dir), 0o700);
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
