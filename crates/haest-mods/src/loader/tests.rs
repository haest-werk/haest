use haest_core::{Tools, workspace::WorkspaceMode};
use serde_json::json;

use super::*;
use crate::{ask::Decision, decorate::ModTools, testing::*};

#[test]
fn the_built_ins_load_first_and_approvals_can_be_turned_off() {
    let all = load_mods(ModOptions::new(CWD, WorkspaceMode::Real), TestUi::new());
    assert_eq!(all.names(), ["approval", "plan"]);
    let quiet = ModOptions::new(CWD, WorkspaceMode::Real).with_approvals(false);
    let no_approvals = load_mods(quiet, TestUi::new());
    assert_eq!(no_approvals.names(), ["plan"]);
    assert!(no_approvals.has_command(crate::plan::PLAN_COMMAND));
}

/// A native mod of the host's, known by its name.
struct Hosts(&'static str);

#[async_trait::async_trait]
impl crate::api::Mod for Hosts {
    fn name(&self) -> &str {
        self.0
    }
}

#[test]
fn the_hosts_own_mods_load_last_in_order() {
    let with = ModOptions::new(CWD, WorkspaceMode::Real)
        .with_mod(Hosts("first"))
        .with_mod(Hosts("second"));
    let mods = load_mods(with, TestUi::new());
    assert_eq!(mods.names(), ["approval", "plan", "first", "second"]);
    let bare = ModOptions::new(CWD, WorkspaceMode::Real)
        .with_approvals(false)
        .with_mod(Hosts("only"));
    assert_eq!(load_mods(bare, TestUi::new()).names(), ["plan", "only"]);
}

#[tokio::test]
async fn the_hosts_tool_names_reach_the_built_ins() {
    let ui = TestUi::answering([Decision::Deny]);
    let named = ModOptions::new(CWD, WorkspaceMode::Real)
        .with_approval(ApprovalMod::new().with_shell_tools(["Shell"]))
        .with_plan(PlanMod::new().with_planning_tools(["Notes"]));
    let mods = load_mods(named, ui.clone());
    let (shell, notes) = (Probe::new("Shell"), Probe::new("Notes"));
    let tools = ModTools::wrap(&mods, Tools::from_iter([shell.clone(), notes.clone()]));
    let rm = input(json!({"command": "rm -rf x"}));
    assert!(tools[0].execute(rm, &ctx()).await.is_err());
    assert_eq!(ui.asks()[0].tool_name, "Shell");
    mods.command("plan", "").await;
    tools[1].execute(input(json!({})), &ctx()).await.unwrap();
    assert_eq!(notes.ran().len(), 1);
}
