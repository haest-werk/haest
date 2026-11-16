//! Which workspace, which processes, and what of the host's environment they get.

use std::{collections::BTreeMap, time::Duration};

use haest_core::workspace::ExecOptions;
use tokio_util::sync::CancellationToken;

use crate::{
    haest::{core::fs::MemFs, tools::inbox::NoInbox},
    testkit::{hooks, settings},
};

use super::*;

fn vars(pairs: &[(&str, &str)]) -> ChildEnv {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A dir of this test run's own on the real disk (not made yet).
fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("haest-world-{name}-{}", std::process::id()))
}

/// What `script` prints in `ws`.
async fn run(ws: &dyn Workspace, script: &str) -> String {
    let opts = ExecOptions {
        cwd: std::env::temp_dir(),
        cancel: CancellationToken::new(),
        timeout: Duration::from_secs(10),
    };
    let out = ws.shell(script, opts).await;
    assert_eq!(out.exit_code, 0, "{script}: {}", out.stderr);
    out.stdout
}

#[tokio::test]
async fn the_sandbox_shell_sees_the_passed_variables_and_none_of_the_hosts_own() {
    let world = World::Fs(Arc::new(MemFs::new()));
    let passed = vars(&[("GH_TOKEN", "t0k")]);
    let ws = workspace_for(
        &settings(),
        &world,
        &passed,
        &[],
        Path::new("/d/spill"),
        Remedies::default(),
    )
    .await
    .unwrap();
    let out = run(ws.as_ref(), "printf '%s|%s' \"$GH_TOKEN\" \"$CARGO_HOME\"").await;
    assert_eq!(out, "t0k|");
}

#[tokio::test]
async fn real_bash_gets_the_passed_variables_over_child_env() {
    let path = std::env::var("PATH").unwrap();
    let world = World::Disk {
        child_env: vars(&[("PATH", &path), ("SHARED", "host")]),
    };
    let s = Settings {
        workspace: WorkspaceMode::Real,
        ..settings()
    };
    let passed = vars(&[("SHARED", "passed"), ("GH_TOKEN", "t0k")]);
    let spill = scratch("real-bash");
    let ws = workspace_for(&s, &world, &passed, &[], &spill, Remedies::default())
        .await
        .unwrap();
    let out = run(ws.as_ref(), "printf '%s|%s' \"$SHARED\" \"$GH_TOKEN\"").await;
    let _ = std::fs::remove_dir_all(&spill);
    assert_eq!(out, "passed|t0k");
    let processes = world.child_env(&passed).unwrap();
    assert_eq!(processes.get("PATH"), Some(path.as_str()));
    assert_eq!(World::Fs(Arc::new(MemFs::new())).child_env(&passed), None);
}

#[tokio::test]
async fn a_world_of_the_hosts_files_starts_no_stdio_server_and_says_which() {
    let (hooks, ui) = hooks(settings());
    let server = McpServerConfig {
        name: "gh".into(),
        transport: McpTransport::Stdio {
            command: "gh-mcp".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        },
    };
    let world = World::Fs(Arc::new(MemFs::new()));
    let hub = connect_mcp(&hooks, &world, &ChildEnv::default(), vec![server]).await;
    assert!(hub.tools().is_empty());
    assert_eq!(ui.warnings().len(), 1, "{:?}", ui.warnings());
}

#[tokio::test]
async fn the_real_workspace_needs_the_real_disk() {
    let s = Settings {
        workspace: WorkspaceMode::Real,
        ..settings()
    };
    let world = World::Fs(Arc::new(MemFs::new()));
    let none = ChildEnv::default();
    let err = workspace_for(
        &s,
        &world,
        &none,
        &[],
        Path::new("/d/spill"),
        Remedies::default(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, WorkspaceError::RealNeedsDisk), "{err}");
    let sandbox = workspace_for(
        &settings(),
        &world,
        &none,
        &[],
        Path::new("/d/spill"),
        Remedies::default(),
    )
    .await;
    assert!(sandbox.is_ok());
}

/// The sandbox on the disk makes its spill dir first: when it cannot, the
/// error says which dir (a bare "Not a directory" names nothing to fix).
#[tokio::test]
async fn a_spill_dir_that_cannot_be_made_is_named_in_the_error() {
    let world = World::Disk {
        child_env: ChildEnv::default(),
    };
    let spill = Path::new("/dev/null/spill");
    let err = workspace_for(
        &settings(),
        &world,
        &ChildEnv::default(),
        &[],
        spill,
        Remedies::default(),
    )
    .await
    .err()
    .unwrap();
    assert!(
        matches!(&err, WorkspaceError::SpillDir { path, .. } if path == spill),
        "{err:?}"
    );
}

/// Spilled output is file contents and command output: on the disk its dir
/// is private (0700), as the session dir is, whichever workspace spills
/// into it (the spill itself makes the dir only when it is missing).
#[cfg(unix)]
#[tokio::test]
async fn the_spill_dir_on_the_disk_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("spill-mode");
    let world = World::Disk {
        child_env: ChildEnv::default(),
    };
    for (mode, name) in [
        (WorkspaceMode::Sandbox, "sandbox"),
        (WorkspaceMode::Real, "real"),
    ] {
        let cwd = root.join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        let s = Settings {
            workspace: mode,
            cwd: cwd.clone(),
            ..settings()
        };
        let spill = cwd.join("data/spill/session");
        let ws = workspace_for(
            &s,
            &world,
            &ChildEnv::default(),
            &[],
            &spill,
            Remedies::default(),
        )
        .await
        .unwrap();
        ensure_dir(ws.fs().as_ref(), &spill).await.unwrap();
        let bits = std::fs::metadata(&spill).unwrap().permissions().mode() & 0o777;
        assert_eq!(bits, 0o700, "{name} workspace: {bits:o}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A skill dir in the project is the project's: the sandbox edits it in its
/// overlay, as any project file, and the edit is a change to review. A skill
/// dir outside it is mounted to read, never written.
#[tokio::test]
async fn the_sandbox_edits_project_skills_and_only_reads_the_others() {
    let root = scratch("skill-mounts");
    let (cwd, home_skills) = (root.join("project"), root.join("home/skills"));
    let project_skills = cwd.join(".claude/skills");
    for dir in [project_skills.join("tidy"), home_skills.join("lint")] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
    }
    let s = Settings {
        cwd: cwd.clone(),
        ..settings()
    };
    let world = World::Disk {
        child_env: ChildEnv::default(),
    };
    let ws = workspace_for(
        &s,
        &world,
        &ChildEnv::default(),
        &[project_skills.clone(), home_skills.clone()],
        &root.join("spill"),
        Remedies::default(),
    )
    .await
    .unwrap();
    let fs = ws.fs();
    let edited = fs
        .write(&project_skills.join("tidy/SKILL.md"), b"edited")
        .await;
    let outside = fs
        .write(&home_skills.join("lint/SKILL.md"), b"edited")
        .await;
    let changed = ws.changes().unwrap().list().await.len();
    let on_disk = std::fs::read_to_string(project_skills.join("tidy/SKILL.md")).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        edited.is_ok(),
        "the project skill was not writable: {edited:?}"
    );
    assert_eq!(changed, 1, "the edit is one change to review");
    assert_eq!(on_disk, "---\nname: x\n---\n", "the disk is untouched");
    assert!(outside.is_err(), "a skill outside the project was written");
}

#[tokio::test]
async fn a_shut_workspace_is_shut_in_any_world() {
    let s = Settings {
        workspace: WorkspaceMode::Shut,
        ..settings()
    };
    let worlds = [
        World::Fs(Arc::new(MemFs::new())),
        World::Disk {
            child_env: ChildEnv::default(),
        },
    ];
    for world in worlds {
        let ws = workspace_for(
            &s,
            &world,
            &ChildEnv::default(),
            &[],
            Path::new("/d/spill"),
            Remedies::default(),
        )
        .await
        .unwrap();
        assert_eq!(ws.mode(), WorkspaceMode::Shut);
    }
}

#[test]
fn background_jobs_exist_only_for_bash_in_the_real_workspace() {
    let jobs = |workspace, bash| {
        let mut s = Settings {
            workspace,
            ..settings()
        };
        s.tools.bash = bash;
        jobs_for(&s, Arc::new(NoInbox)).is_some()
    };
    assert!(!jobs(WorkspaceMode::Sandbox, true));
    assert!(!jobs(WorkspaceMode::Real, false));
    assert!(jobs(WorkspaceMode::Real, true));
}

#[tokio::test]
async fn killing_at_exit_counts_the_jobs_and_waits_until_they_are_gone() {
    let jobs = JobHub::new(Arc::new(NoInbox));
    let env: ChildEnv = std::env::vars().collect();
    let ws: Arc<dyn Workspace> = Arc::new(RealWorkspace::new(env));
    let cwd = std::env::temp_dir();
    for _ in 0..2 {
        jobs.start(ws.clone(), "sleep 30", "s", cwd.clone())
            .unwrap();
    }
    assert_eq!(kill_jobs(&jobs).await, 2);
    assert!(jobs.running().is_empty());
    let ended = ["b1", "b2"].map(|id| jobs.get(id).and_then(|j| j.exit_code));
    assert!(ended.iter().all(Option::is_some), "{ended:?}");
    assert_eq!(kill_jobs(&jobs).await, 0);
}
