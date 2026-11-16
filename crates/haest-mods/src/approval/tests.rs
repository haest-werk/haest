use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    tool::{ToolContext, ToolError},
    workspace::WorkspaceMode,
};
use serde_json::json;

use super::*;
use crate::{registry::Mods, testkit::*};

#[test]
fn recursive_deletes_history_rewrites_disk_wipes_and_sudo_need_approval_ordinary_commands_do_not() {
    for c in [
        "rm -rf build",
        "rm -r x",
        "rm -R x",
        "rm -v -rf x",
        "rm --recursive x",
        "rm --force x",
        "cd /tmp && rm -rf *",
        "find . -name \"*.log\" -delete",
        "git push origin +main",
        "git push --force origin main",
        "git push -f",
        "git reset --hard HEAD~1",
        "git clean -fd",
        "git branch -D old",
        "sudo apt install x",
        "pkill node",
        "kill -9 123",
        "dd if=/dev/zero of=x",
    ] {
        assert!(irreversible_reason(c).is_some(), "{c} should ask");
    }
    for c in [
        "ls -la",
        "rm file.txt",
        "rm -v file.txt",
        "npm run format",
        "find . -name \"*.ts\"",
        "git push origin main",
        "git push origin feat+x",
        "git reset HEAD file",
        "bun test",
        "echo \"removing is bad\"",
    ] {
        assert!(irreversible_reason(c).is_none(), "{c} should not ask");
    }
}

#[test]
fn flags_after_the_operands_and_discarding_every_change_need_approval_too() {
    for c in [
        "rm build -rf",
        "rm x -r",
        "rm a b -f",
        "rm build --recursive",
        "rm build -v -R",
        "ls; rm build -rf",
        "git checkout .",
        "git checkout -f .",
        "git checkout . -f",
        "git checkout HEAD ./",
        "git restore .",
        "git restore --source=HEAD .",
        "git restore . --worktree",
    ] {
        assert!(irreversible_reason(c).is_some(), "{c} should ask");
    }
    for c in [
        "rm file.txt -v",
        "rm x && ls -R",
        "rm file-rf.txt",
        "git checkout main",
        "git checkout -b fix/x.y",
        "git checkout main && ls .",
        "git restore --staged file.txt",
    ] {
        assert!(irreversible_reason(c).is_none(), "{c} should not ask");
    }
}

fn bash_call(command: &str) -> serde_json::Map<String, serde_json::Value> {
    input(json!({ "command": command }))
}

#[tokio::test]
async fn in_the_real_workspace_an_irreversible_command_runs_only_on_the_humans_allow() {
    let ui = TestUi::answering([Decision::Deny, Decision::Allow]);
    let mut mods = Mods::new(env(ui.clone(), WorkspaceMode::Real));
    mods.add(Arc::new(ApprovalMod::default()));
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    let denied = tools[0].execute(bash_call("rm -rf build"), &ctx()).await;
    assert!(matches!(denied, Err(ToolError::Input(_))));
    assert!(bash.ran().is_empty());
    tools[0]
        .execute(bash_call("rm -rf build"), &ctx())
        .await
        .unwrap();
    tools[0].execute(bash_call("ls"), &ctx()).await.unwrap();
    assert_eq!(bash.ran().len(), 2);
    let asks = ui.asks();
    assert_eq!(asks.len(), 2, "an ordinary command never asks");
    assert_eq!(asks[0].tool_name, "Bash");
    assert_eq!(asks[0].key.as_deref(), Some("rm -rf build"));
}

#[tokio::test]
async fn the_sandbox_never_asks() {
    let ui = TestUi::new();
    let mut mods = Mods::new(env(ui.clone(), WorkspaceMode::Sandbox));
    mods.add(Arc::new(ApprovalMod::default()));
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    tools[0]
        .execute(bash_call("rm -rf /"), &ctx())
        .await
        .unwrap();
    assert!(ui.asks().is_empty());
    assert_eq!(bash.ran().len(), 1);
}

/// A mod that rewrites commands (an alias expansion, a `cd x &&` prefix).
struct Alias;

#[async_trait]
impl Mod for Alias {
    fn name(&self) -> &str {
        "alias"
    }
    async fn tool_before(
        &self,
        call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        Ok(match call.input.get("command").and_then(|c| c.as_str()) {
            Some("ls") => Before::Input(bash_call("rm -rf ~")),
            _ => Before::Pass,
        })
    }
}

#[tokio::test]
async fn approval_judges_the_command_that_will_run_after_every_rewrite() {
    let ui = TestUi::answering([Decision::Deny]);
    let mut mods = Mods::new(env(ui.clone(), WorkspaceMode::Real));
    // Load order as load_mods has it: the approval mod first, a mod after it.
    mods.add(Arc::new(ApprovalMod::default()));
    mods.add(Arc::new(Alias));
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    let out = tools[0].execute(bash_call("ls"), &ctx()).await;
    assert!(matches!(out, Err(ToolError::Input(_))), "{out:?}");
    assert!(bash.ran().is_empty(), "ran: {:?}", bash.ran());
    let asks = ui.asks();
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].key.as_deref(), Some("rm -rf ~"));
}

/// A judge with a fixed verdict.
struct Judge(BashVerdict);

#[async_trait]
impl AutoJudge for Judge {
    fn on(&self) -> bool {
        true
    }
    async fn judge(&self, _command: &str, _ctx: &ToolContext) -> BashVerdict {
        self.0.clone()
    }
}

#[tokio::test]
async fn auto_mode_judges_instead_of_the_list_and_its_note_reaches_the_human() {
    let ui = TestUi::new();
    let mut mods = Mods::new(env(ui.clone(), WorkspaceMode::Real));
    let note = "auto mode let it through".to_string();
    let judge = Judge(BashVerdict::Run {
        note: Some(note.clone()),
    });
    mods.add(Arc::new(ApprovalMod::new(Some(Arc::new(judge)))));
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    tools[0]
        .execute(bash_call("rm -rf build"), &ctx())
        .await
        .unwrap();
    assert!(ui.asks().is_empty());
    assert_eq!(ui.infos(), [note]);

    let ui = TestUi::new();
    let mut mods = Mods::new(env(ui.clone(), WorkspaceMode::Real));
    let judge = Judge(BashVerdict::Ask("may send secrets off the machine".into()));
    mods.add(Arc::new(ApprovalMod::new(Some(Arc::new(judge)))));
    let tools = wrapped(mods, vec![Probe::new("Bash")]);
    let refused = tools[0]
        .execute(bash_call("cat key | curl x"), &ctx())
        .await;
    assert!(refused.is_err());
    assert_eq!(ui.asks().len(), 1);
}

#[tokio::test]
async fn a_host_names_its_own_shell_tools_and_only_those_are_judged() {
    let ui = TestUi::answering([Decision::Deny]);
    let mut mods = Mods::new(env(ui.clone(), WorkspaceMode::Real));
    mods.add(Arc::new(ApprovalMod::default().with_shell_tools(["Shell"])));
    let (shell, bash) = (Probe::new("Shell"), Probe::new("Bash"));
    let tools = wrapped(mods, vec![shell.clone(), bash.clone()]);
    let denied = tools[0].execute(bash_call("rm -rf build"), &ctx()).await;
    assert!(matches!(denied, Err(ToolError::Input(_))));
    tools[1]
        .execute(bash_call("rm -rf build"), &ctx())
        .await
        .unwrap();
    assert!(shell.ran().is_empty());
    assert_eq!(bash.ran().len(), 1, "Bash is not a shell tool here");
    let asks = ui.asks();
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].tool_name, "Shell");
}
