//! Grep through the sandbox's rg (bashkit): the argv the tool builds is one it
//! takes, and a target that looks like a flag is still a path there.

use std::{path::Path, sync::Arc};

use haest_core::{
    fs::MemFs,
    tool::{Tool, ToolContext},
};
use haest_workspace::SandboxWorkspace;
use serde_json::json;

use super::super::GrepTool;
use crate::testkit::{CWD, input};

fn sandbox<P: AsRef<Path>>(files: impl IntoIterator<Item = (P, &'static str)>) -> ToolContext {
    let fs = Arc::new(MemFs::with_files(files));
    ToolContext::new(CWD, Arc::new(SandboxWorkspace::over(fs)))
}

#[tokio::test]
async fn the_sandbox_rg_takes_the_argv_and_searches_a_flag_like_target_as_a_path() {
    let c = sandbox([
        ("/p/src/a.ts", "const needle = 1\n"),
        ("/p/--pre=sh", "needle\n"),
    ]);
    let content = GrepTool
        .execute(
            input(json!({"pattern": "needle", "path": "src", "output_mode": "content"})),
            &c,
        )
        .await
        .unwrap();
    assert!(content.content.contains("a.ts:1:const needle"));
    let flag_like = GrepTool
        .execute(input(json!({"pattern": "needle", "path": "--pre=sh"})), &c)
        .await
        .unwrap();
    let printed = flag_like.content.trim();
    assert_eq!(printed.strip_prefix("./").unwrap_or(printed), "--pre=sh");
}
