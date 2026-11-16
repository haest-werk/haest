use std::path::PathBuf;

use haest_core::{fs::MemFs, workspace::WorkspaceMode};
use serde_json::json;

use super::*;
use crate::testkit::*;

const HOME_MODS: &str = "/home/cfg/mods";

fn cfg(workspace: WorkspaceMode) -> ModConfig {
    let mut cfg = ModConfig::new(CWD, workspace, "/home/cfg");
    cfg.user_mods = true;
    cfg
}

#[test]
fn no_mod_dir_is_read_unless_asked_for() {
    let bare = ModConfig::new(CWD, WorkspaceMode::Sandbox, "/home/cfg");
    assert!(bare.mod_dirs().is_empty());
    assert_eq!(
        cfg(WorkspaceMode::Sandbox).mod_dirs(),
        [PathBuf::from(HOME_MODS)]
    );
}

#[tokio::test]
async fn a_mod_in_the_user_mods_dir_loads_and_a_broken_one_is_reported_and_skipped() {
    let fs = MemFs::with_files([
        (
            format!("{HOME_MODS}/a-good.ts"),
            "export default function (haest) { haest.tool({ name: 'Hello', kind: 'agent.unknown', description: 'hi', parameters: {}, execute: async () => ({ content: 'hello' }) }) }",
        ),
        (format!("{HOME_MODS}/b-broken.ts"), "export default 42"),
    ]);
    let host = TestHost::new();
    let mods = load_mods_with(
        cfg(WorkspaceMode::Sandbox),
        &fs,
        host.clone(),
        test_limits(),
    )
    .await;
    let tools = wrapped(mods, Vec::new());
    let hello = find(&tools, "Hello");
    let out = hello.execute(input(json!({})), &ctx()).await.unwrap();
    assert_eq!(out.content, "hello");
    let errors = host.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("b-broken.ts"));
}

#[tokio::test]
async fn mod_files_are_code_files_in_name_order_and_declarations_are_not_mods() {
    let fs = MemFs::with_files([
        ("/m/b.js", "x"),
        ("/m/a.ts", "x"),
        ("/m/c.mjs", "x"),
        ("/m/types.d.ts", "x"),
        ("/m/notes.md", "x"),
    ]);
    let names: Vec<PathBuf> = mod_files(&fs, Path::new("/m")).await;
    assert_eq!(names, ["/m/a.ts", "/m/b.js", "/m/c.mjs"].map(PathBuf::from));
    assert!(mod_files(&fs, Path::new("/missing")).await.is_empty());
}

#[tokio::test]
async fn project_mods_load_only_when_the_host_turns_them_on() {
    let source =
        "export default function (haest) { haest.on('system.prompt', () => 'from the repo') }";
    let fs = MemFs::with_files([(format!("{CWD}/.haest/mods/repo.js"), source)]);
    let off = load_mods_with(
        cfg(WorkspaceMode::Sandbox),
        &fs,
        TestHost::new(),
        test_limits(),
    )
    .await;
    assert_eq!(off.system_append().await, "");
    let mut on_cfg = cfg(WorkspaceMode::Sandbox);
    on_cfg.project_mods = true;
    let on = load_mods_with(on_cfg, &fs, TestHost::new(), test_limits()).await;
    assert_eq!(on.system_append().await, "from the repo");
}

#[tokio::test]
async fn the_built_ins_load_first_and_approvals_can_be_turned_off() {
    let fs = MemFs::new();
    let all = load_mods_with(
        cfg(WorkspaceMode::Real),
        &fs,
        TestHost::new(),
        test_limits(),
    )
    .await;
    assert_eq!(all.names(), ["approval", "plan"]);
    let mut quiet = cfg(WorkspaceMode::Real);
    quiet.approvals = false;
    let no_approvals = load_mods_with(quiet, &fs, TestHost::new(), test_limits()).await;
    assert_eq!(no_approvals.names(), ["plan"]);
    assert!(no_approvals.has_command(crate::plan::PLAN_COMMAND));
}
