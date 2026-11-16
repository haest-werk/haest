//! Glob, ported from tools/tools.test.ts: walks the workspace Fs, newest first, capped.

use std::time::Duration;

use haest_core::tool::Tool;
use serde_json::json;

use super::super::GlobTool;
use crate::{
    files::WriteTool,
    testkit::{TestWorkspace, ctx, input},
};

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
