//! Ported from tools/tools.test.ts (Grep; Glob in `glob.rs`). Grep runs against
//! the REAL rg when it is installed and the sandbox's rg (`sandbox.rs`): the argv
//! must be one both accept. The exit-code and output handling is pinned with a
//! scripted workspace.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use kern_core::{
    fs::{Fs, MemFs},
    tool::{Tool, ToolContext, ToolError, ToolOutput},
    workspace::{ExecOptions, ExecResult, Workspace, WorkspaceMode},
};
use serde_json::{Value, json};

use super::GrepTool;
use crate::testkit::{TestWorkspace, ctx, input, stdout};

mod glob;
mod sandbox;

/// Programs run for real in a directory on disk: the only way to know rg takes our argv.
struct HostRun;

#[async_trait]
impl Workspace for HostRun {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Real
    }
    fn fs(&self) -> Arc<dyn Fs> {
        Arc::new(MemFs::new())
    }
    async fn shell(&self, _: &str, _: ExecOptions) -> ExecResult {
        unreachable!("Grep never runs a shell")
    }
    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        let out = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&opts.cwd)
            .output()
            .await
            .expect("spawn");
        ExecResult {
            stdout: String::from_utf8_lossy(&out.stdout).into(),
            stderr: String::from_utf8_lossy(&out.stderr).into(),
            exit_code: out.status.code().unwrap_or(-1),
            timed_out: false,
        }
    }
    fn prompt_note(&self) -> String {
        String::new()
    }
}

/// The TS fixture on disk: src/a.ts has the needle, src/b.md does not, node_modules has one too.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let put = |rel: &str, text: &str| {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    put("src/a.ts", "const needle = 1\n");
    put("src/b.md", "no match here\n");
    put("node_modules/x/c.ts", "needle\n");
    put("src/flags.txt", "use -rf here\n");
    dir
}

fn has_rg() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

async fn grep_real(dir: &Path, v: Value) -> Result<ToolOutput, ToolError> {
    GrepTool
        .execute(input(v), &ToolContext::new(dir, Arc::new(HostRun)))
        .await
}

#[tokio::test]
async fn grep_finds_files_content_honours_glob_and_takes_a_pattern_that_starts_with_a_dash() {
    if !has_rg() {
        eprintln!("rg is not installed: skipping the real-rg grep test");
        return;
    }
    let fx = fixture();
    let dir = fx.path();
    let files = grep_real(dir, json!({"pattern": "needle"})).await.unwrap();
    assert!(files.content.contains("src/a.ts"));
    let content = grep_real(
        dir,
        json!({"pattern": "needle", "output_mode": "content", "glob": "*.ts"}),
    )
    .await
    .unwrap();
    assert!(content.content.contains(":1:const needle"));
    let none = grep_real(dir, json!({"pattern": "zzz"})).await.unwrap();
    assert!(!none.is_error);
    assert!(!none.content.contains("src/"));
    let dash = grep_real(dir, json!({"pattern": "-rf"})).await.unwrap();
    assert!(dash.content.contains("flags.txt"));
}

#[tokio::test]
async fn a_path_that_looks_like_a_flag_is_searched_as_a_path_and_runs_nothing() {
    if !has_rg() {
        eprintln!("rg is not installed: skipping the real-rg grep test");
        return;
    }
    let fx = fixture();
    let dir = fx.path();
    let ran = dir.join("ran");
    // Read as a flag, `--pre=sh` runs every file under cwd through sh: this one leaves a mark.
    std::fs::write(dir.join("mark.sh"), format!("touch '{}'\n", ran.display())).unwrap();
    let missing = grep_real(dir, json!({"pattern": "x", "path": "--pre=sh"})).await;
    assert!(!ran.exists(), "rg ran the files under cwd as scripts");
    assert!(matches!(missing, Err(ToolError::Input(_))));
    std::fs::write(dir.join("--pre=sh"), "x\n").unwrap();
    let found = grep_real(dir, json!({"pattern": "x", "path": "--pre=sh"}))
        .await
        .unwrap();
    assert_eq!(
        same_file(dir, found.content.trim()),
        same_file(dir, "--pre=sh")
    );
    assert!(!ran.exists());
}

/// Where a path rg printed (relative to `dir`) really is.
fn same_file(dir: &Path, printed: &str) -> PathBuf {
    std::fs::canonicalize(dir.join(printed)).expect("rg printed a real path")
}

fn rg_answering(answer: ExecResult) -> Arc<TestWorkspace> {
    TestWorkspace::answering([("/p/.keep", "")], move |_| answer.clone())
}

#[tokio::test]
async fn grep_builds_one_argv_with_the_pattern_behind_e() {
    let ws = rg_answering(stdout("a.ts\n"));
    let call = json!({"pattern": "x", "path": "src", "output_mode": "content", "case_insensitive": true,
        "glob": "*.ts", "context": 2.7, "multiline": "true"});
    GrepTool
        .execute(input(call), &ctx(ws.clone()))
        .await
        .unwrap();
    let calls = ws.calls();
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].shell);
    assert_eq!(calls[0].cwd, PathBuf::from("/p"));
    assert_eq!(
        calls[0].argv,
        [
            "rg",
            "-n",
            "-i",
            "-U",
            "--multiline-dotall",
            "--glob",
            "*.ts",
            "-C",
            "2",
            "-e",
            "x",
            "src"
        ]
    );
    GrepTool
        .execute(
            input(json!({"pattern": "y", "context": 3})),
            &ctx(ws.clone()),
        )
        .await
        .unwrap();
    assert_eq!(ws.calls()[1].argv, ["rg", "-l", "-e", "y", "."]);
    // A target rg would read as a flag, or a bare `-` it would read as stdin, names a file.
    for (path, arg) in [("--pre=rm", "./--pre=rm"), ("-", "./-")] {
        GrepTool
            .execute(
                input(json!({"pattern": "y", "path": path})),
                &ctx(ws.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            ws.calls().last().unwrap().argv,
            ["rg", "-l", "-e", "y", arg]
        );
    }
    let bad = GrepTool
        .execute(
            input(json!({"pattern": "y", "output_mode": "lines"})),
            &ctx(ws),
        )
        .await;
    assert!(matches!(bad, Err(ToolError::Input(_))));
}

async fn grep_with(answer: ExecResult) -> Result<ToolOutput, ToolError> {
    GrepTool
        .execute(input(json!({"pattern": "x"})), &ctx(rg_answering(answer)))
        .await
}

#[tokio::test]
async fn exit_one_alone_is_no_match_but_a_complaint_or_a_missing_rg_is_an_error() {
    let fail = |code: i32, stderr: &str| ExecResult {
        exit_code: code,
        stderr: stderr.into(),
        ..Default::default()
    };
    let none = grep_with(fail(1, " \n")).await.unwrap();
    assert!(!none.is_error);
    assert!(matches!(
        grep_with(fail(1, "unknown flag")).await,
        Err(ToolError::Input(_))
    ));
    assert!(matches!(
        grep_with(fail(2, "")).await,
        Err(ToolError::Input(_))
    ));
    assert!(matches!(
        grep_with(fail(127, "")).await,
        Err(ToolError::Input(_))
    ));
}

#[tokio::test]
async fn matches_survive_files_rg_could_not_read_and_the_model_is_told_which() {
    let partial = ExecResult {
        stdout: "src/a.ts\n".into(),
        stderr: "rg: src/locked.ts: Permission denied (os error 13)\n".into(),
        exit_code: 2,
        ..Default::default()
    };
    let r = grep_with(partial).await.unwrap();
    assert!(!r.is_error);
    assert_eq!(r.content.lines().next(), Some("src/a.ts"));
    assert!(r.content.contains("src/locked.ts"));
}

#[tokio::test]
async fn the_real_rg_reports_what_it_found_beside_an_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;
    if !has_rg() {
        eprintln!("rg is not installed: skipping the real-rg grep test");
        return;
    }
    let fx = fixture();
    let locked = fx.path().join("src/locked.ts");
    std::fs::write(&locked, "needle\n").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked).is_ok() {
        eprintln!("running as a user who reads mode-000 files: skipping");
        return;
    }
    let r = grep_real(fx.path(), json!({"pattern": "needle", "path": "src"})).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    let r = r.unwrap();
    assert!(r.content.contains("src/a.ts"));
    assert!(r.content.contains("locked.ts"));
}

#[tokio::test]
async fn head_limit_keeps_the_first_lines_and_long_lines_are_clipped() {
    let long = "y".repeat(2000);
    let ws = rg_answering(stdout(&format!("a:1:x\nb:2:x\n\nc:3:{long}\n")));
    let r = GrepTool
        .execute(
            input(json!({"pattern": "x", "head_limit": 2})),
            &ctx(ws.clone()),
        )
        .await
        .unwrap();
    let lines: Vec<_> = r.content.lines().collect();
    assert_eq!(lines[..2], ["a:1:x", "b:2:x"]);
    assert!(!r.content.contains("c:3"));
    let all = GrepTool
        .execute(input(json!({"pattern": "x"})), &ctx(ws))
        .await
        .unwrap();
    let c = all.content.lines().find(|l| l.starts_with("c:3:")).unwrap();
    assert!(c.chars().count() < 600);
}
