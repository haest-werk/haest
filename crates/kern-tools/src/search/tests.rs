//! Ported from tools/tools.test.ts (Grep + Glob). Grep runs against the REAL rg
//! when it is installed (the argv must be one rg accepts); the exit-code and
//! output handling is pinned with a scripted workspace.

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use kern_core::{
    fs::{Fs, MemFs},
    tool::{Tool, ToolContext, ToolError, ToolOutput},
    workspace::{ExecOptions, ExecResult, Workspace, WorkspaceMode},
};
use serde_json::{Value, json};

use super::{GlobTool, GrepTool};
use crate::{
    files::WriteTool,
    testkit::{TestWorkspace, ctx, input, stdout},
};

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
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kern-tools-{}-{nanos}", std::process::id()));
        let put = |rel: &str, text: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put("src/a.ts", "const needle = 1\n");
        put("src/b.md", "no match here\n");
        put("node_modules/x/c.ts", "needle\n");
        put("src/flags.txt", "use -rf here\n");
        Self(dir)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn has_rg() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

async fn grep_real(dir: &PathBuf, v: Value) -> Result<ToolOutput, ToolError> {
    GrepTool
        .execute(input(v), &ToolContext::new(dir, Arc::new(HostRun)))
        .await
}

#[tokio::test]
async fn grep_finds_files_content_and_honours_glob() {
    if !has_rg() {
        eprintln!("rg is not installed: skipping the real-rg grep test");
        return;
    }
    let fx = Fixture::new();
    let files = grep_real(&fx.0, json!({"pattern": "needle"}))
        .await
        .unwrap();
    assert!(files.content.contains("src/a.ts"));
    let content = grep_real(
        &fx.0,
        json!({"pattern": "needle", "output_mode": "content", "glob": "*.ts"}),
    )
    .await
    .unwrap();
    assert!(content.content.contains(":1:const needle"));
    let none = grep_real(&fx.0, json!({"pattern": "zzz"})).await.unwrap();
    assert!(!none.is_error);
    assert!(!none.content.contains("src/"));
}

#[tokio::test]
async fn grep_takes_a_pattern_that_starts_with_a_dash() {
    if !has_rg() {
        eprintln!("rg is not installed: skipping the real-rg grep test");
        return;
    }
    let fx = Fixture::new();
    let r = grep_real(&fx.0, json!({"pattern": "-rf"})).await.unwrap();
    assert!(r.content.contains("flags.txt"));
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

#[tokio::test]
async fn glob_skips_node_modules_and_finds_files_write_created() {
    let ws = TestWorkspace::with_files([
        ("/p/src/a.ts", "const needle = 1\n"),
        ("/p/src/b.md", "no match here\n"),
        ("/p/node_modules/x/c.ts", "needle\n"),
    ]);
    let c = ctx(ws);
    WriteTool
        .execute(
            input(json!({"file_path": "src/new.ts", "content": "x"})),
            &c,
        )
        .await
        .unwrap();
    let r = GlobTool
        .execute(input(json!({"pattern": "**/*.ts"})), &c)
        .await
        .unwrap();
    assert!(r.content.contains("src/a.ts"));
    assert!(r.content.contains("src/new.ts"));
    assert!(!r.content.contains("node_modules"));
}

#[tokio::test]
async fn glob_lists_the_newest_first_and_caps_the_list() {
    let ws = TestWorkspace::with_files([("/p/old.txt", "")]);
    let c = ctx(ws);
    tokio::time::sleep(Duration::from_millis(5)).await;
    WriteTool
        .execute(input(json!({"file_path": "new.txt", "content": ""})), &c)
        .await
        .unwrap();
    let r = GlobTool
        .execute(input(json!({"pattern": "*.txt"})), &c)
        .await
        .unwrap();
    assert_eq!(
        r.content.lines().collect::<Vec<_>>(),
        ["/p/new.txt", "/p/old.txt"]
    );
    let many = TestWorkspace::with_files((0..250).map(|i| (format!("/q/f{i}.ts"), "")));
    let r = GlobTool
        .execute(input(json!({"pattern": "*.ts", "path": "/q"})), &ctx(many))
        .await
        .unwrap();
    assert_eq!(
        r.content.lines().filter(|l| l.starts_with("/q/")).count(),
        200
    );
    let note = r.content.lines().last().unwrap();
    assert!(!note.starts_with("/q/") && note.contains("50"));
}
