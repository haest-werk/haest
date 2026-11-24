//! Write and Edit.

use std::{path::PathBuf, sync::Arc};

use haest_core::{
    fs::Fs,
    tool::{Tool, ToolError, ToolOutput},
};
use serde_json::{Value, json};

use super::{EditTool, WriteTool};
use crate::{
    read::ReadTool,
    testkit::{ClaimedSizeFs, TestWorkspace, Write, ctx, input},
};

async fn run(tool: &dyn Tool, ws: &Arc<TestWorkspace>, v: Value) -> Result<ToolOutput, ToolError> {
    tool.execute(input(v), &ctx(ws.clone())).await
}

fn empty() -> Arc<TestWorkspace> {
    TestWorkspace::from_files([("/p/.keep", "")])
}

#[tokio::test]
async fn write_creates_parent_dirs_and_read_sees_what_write_wrote() {
    let ws = empty();
    run(
        &WriteTool,
        &ws,
        json!({"file_path": "a/b/c.txt", "content": "hi"}),
    )
    .await
    .unwrap();
    assert_eq!(ws.contents("/p/a/b/c.txt").await, "hi");
    let r = run(&ReadTool::default(), &ws, json!({"file_path": "a/b/c.txt"}))
        .await
        .unwrap();
    assert!(r.content.contains("1\thi"));
}

#[tokio::test]
async fn write_takes_empty_content_but_not_a_missing_one() {
    let ws = empty();
    run(
        &WriteTool,
        &ws,
        json!({"file_path": "e.txt", "content": ""}),
    )
    .await
    .unwrap();
    assert_eq!(ws.contents("/p/e.txt").await, "");
    let missing = run(&WriteTool, &ws, json!({"file_path": "f.txt"})).await;
    assert!(matches!(missing, Err(ToolError::Input(_))));
}

#[tokio::test]
async fn edit_requires_a_unique_match_unless_replace_all() {
    let ws = TestWorkspace::from_files([("/p/e.txt", "x y x")]);
    let ambiguous = run(
        &EditTool,
        &ws,
        json!({"file_path": "e.txt", "old_string": "x", "new_string": "z"}),
    )
    .await;
    assert!(matches!(ambiguous, Err(ToolError::Input(_))));
    assert_eq!(ws.contents("/p/e.txt").await, "x y x");
    run(
        &EditTool,
        &ws,
        json!({"file_path": "e.txt", "old_string": "x", "new_string": "z", "replace_all": true}),
    )
    .await
    .unwrap();
    assert_eq!(ws.contents("/p/e.txt").await, "z y z");
    let absent = run(
        &EditTool,
        &ws,
        json!({"file_path": "e.txt", "old_string": "q", "new_string": "r"}),
    )
    .await;
    assert!(matches!(absent, Err(ToolError::Input(_))));
}

#[tokio::test]
async fn edit_replaces_only_the_one_match_it_was_given() {
    let ws = TestWorkspace::from_files([("/p/e.txt", "one two")]);
    run(
        &EditTool,
        &ws,
        json!({"file_path": "e.txt", "old_string": "two", "new_string": ""}),
    )
    .await
    .unwrap();
    assert_eq!(ws.contents("/p/e.txt").await, "one ");
}

#[tokio::test]
async fn edit_treats_dollar_in_the_replacement_literally() {
    let ws = TestWorkspace::from_files([("/p/d.txt", "price")]);
    run(
        &EditTool,
        &ws,
        json!({"file_path": "d.txt", "old_string": "price", "new_string": "$& $1 $$'"}),
    )
    .await
    .unwrap();
    assert_eq!(ws.contents("/p/d.txt").await, "$& $1 $$'");
}

/// An Edit rewrote the file with a plain write, which on the real disk
/// empties it first: a disk that filled up partway left the human's file
/// emptied or cut short, and the model, holding only what it replaced,
/// could not put it back. It asks for the whole write.
#[tokio::test]
async fn edit_replaces_the_file_whole_never_with_a_plain_write() {
    let ws = TestWorkspace::from_files([("/p/e.txt", "one two")]);
    let call = json!({"file_path": "e.txt", "old_string": "two", "new_string": "2"});
    run(&EditTool, &ws, call).await.unwrap();
    assert_eq!(ws.writes(), [(Write::Whole, PathBuf::from("/p/e.txt"))]);
    assert_eq!(ws.contents("/p/e.txt").await, "one 2");
}

/// Write over a file that is there replaced the human's version with a
/// plain write, cut short on a full disk. It asks for the whole write
/// there, and makes a new file with a plain one.
#[tokio::test]
async fn write_replaces_a_file_that_is_there_whole_and_makes_a_new_one() {
    let ws = TestWorkspace::from_files([("/p/e.txt", "the human's")]);
    let over = json!({"file_path": "e.txt", "content": "the model's"});
    run(&WriteTool, &ws, over).await.unwrap();
    let new = json!({"file_path": "n/new.txt", "content": "new"});
    run(&WriteTool, &ws, new).await.unwrap();
    let writes = [
        (Write::Whole, PathBuf::from("/p/e.txt")),
        (Write::Plain, PathBuf::from("/p/n/new.txt")),
    ];
    assert_eq!(ws.writes(), writes);
    assert_eq!(ws.contents("/p/e.txt").await, "the model's");
    assert_eq!(ws.contents("/p/n/new.txt").await, "new");
}

#[tokio::test]
async fn edit_refuses_a_missing_file_and_a_no_op() {
    let ws = TestWorkspace::from_files([("/p/a.txt", "a")]);
    let missing = run(
        &EditTool,
        &ws,
        json!({"file_path": "nope.txt", "old_string": "a", "new_string": "b"}),
    )
    .await;
    assert!(matches!(missing, Err(ToolError::Input(_))));
    assert!(!ws.mem.exists(std::path::Path::new("/p/nope.txt")).await);
    let same = run(
        &EditTool,
        &ws,
        json!({"file_path": "a.txt", "old_string": "a", "new_string": "a"}),
    )
    .await;
    assert!(matches!(same, Err(ToolError::Input(_))));
}

#[test]
fn edit_canonical_input_uses_the_transcript_keys() {
    let c = EditTool.canonical_input(&input(
        json!({"file_path": "a", "old_string": "o", "new_string": "n"}),
    ));
    assert_eq!(
        json!(c),
        json!({"path": "a", "oldText": "o", "newText": "n"})
    );
}

#[tokio::test]
async fn edit_refuses_a_file_over_the_size_cap_before_reading_it() {
    let huge = ClaimedSizeFs::new([("/p/huge.txt", "a\n")], 5 << 30);
    let call = json!({"file_path": "huge.txt", "old_string": "a", "new_string": "b"});
    let r = EditTool.execute(input(call), &huge.ctx()).await;
    assert_eq!(huge.reads(), 0, "Edit loaded a 5 GB file whole");
    assert!(matches!(r, Err(ToolError::Input(_))));
}
