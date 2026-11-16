use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use haest_core::{fs::MemFs, workspace::WorkspaceMode};
use serde_json::json;

use super::*;
use crate::{api::Decision, decorate::ModTools, testkit::*};

const HOME_MODS: &str = "/home/cfg/mods";

fn cfg(workspace: WorkspaceMode) -> ModOptions {
    let mut cfg = ModOptions::new(CWD, workspace, "/home/cfg");
    cfg.user_mods = true;
    cfg.caps = test_caps();
    cfg
}

#[test]
fn no_mod_dir_is_read_unless_asked_for() {
    let bare = ModOptions::new(CWD, WorkspaceMode::Sandbox, "/home/cfg");
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
    let ui = TestUi::new();
    let mods = load_mods(cfg(WorkspaceMode::Sandbox), &fs, ui.clone()).await;
    let tools = wrapped(mods, Vec::new());
    let hello = find(&tools, "Hello");
    let out = hello.execute(input(json!({})), &ctx()).await.unwrap();
    assert_eq!(out.content, "hello");
    let errors = ui.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("b-broken.ts"));
}

#[tokio::test]
async fn the_hosts_limits_bound_every_mod_file_it_loads() {
    let fs = MemFs::with_files([(
        format!("{HOME_MODS}/hangs.js"),
        "export default () => new Promise(() => {})",
    )]);
    let ui = TestUi::new();
    let mut quick = cfg(WorkspaceMode::Sandbox);
    quick.caps.load_timeout = Duration::from_millis(200);
    let started = Instant::now();
    let mods = load_mods(quick, &fs, ui.clone()).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(mods.names(), ["approval", "plan"]);
    assert!(ui.errors()[0].contains("hangs.js"), "{:?}", ui.errors());
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
    let off = load_mods(cfg(WorkspaceMode::Sandbox), &fs, TestUi::new()).await;
    assert_eq!(off.system_append().await, "");
    let mut on_cfg = cfg(WorkspaceMode::Sandbox);
    on_cfg.project_mods = true;
    let on = load_mods(on_cfg, &fs, TestUi::new()).await;
    assert_eq!(on.system_append().await, "from the repo");
}

#[tokio::test]
async fn the_built_ins_load_first_and_approvals_can_be_turned_off() {
    let fs = MemFs::new();
    let all = load_mods(cfg(WorkspaceMode::Real), &fs, TestUi::new()).await;
    assert_eq!(all.names(), ["approval", "plan"]);
    let mut quiet = cfg(WorkspaceMode::Real);
    quiet.approvals = false;
    let no_approvals = load_mods(quiet, &fs, TestUi::new()).await;
    assert_eq!(no_approvals.names(), ["plan"]);
    assert!(no_approvals.has_command(crate::plan::PLAN_COMMAND));
}

#[tokio::test]
async fn the_hosts_tool_names_reach_the_built_ins() {
    let ui = TestUi::answering([Decision::Deny]);
    let mut named = cfg(WorkspaceMode::Real);
    named.shell_tools = vec!["Shell".into()];
    named.planning_tools = vec!["Notes".into()];
    let mods = Arc::new(load_mods(named, &MemFs::new(), ui.clone()).await);
    let (shell, notes) = (Probe::new("Shell"), Probe::new("Notes"));
    let tools = ModTools::wrap(&mods, vec![shell.clone(), notes.clone()]);
    let rm = input(json!({"command": "rm -rf x"}));
    assert!(tools[0].execute(rm, &ctx()).await.is_err());
    assert_eq!(ui.asks()[0].tool_name, "Shell");
    mods.command("plan", "").await;
    tools[1].execute(input(json!({})), &ctx()).await.unwrap();
    assert_eq!(notes.ran().len(), 1);
}
