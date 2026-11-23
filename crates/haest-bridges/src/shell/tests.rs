//! What only a command does: Claude Code hook scripts as they are, exit
//! codes, the binding's `on_error` on a broken command, the directory and
//! variables it runs with, and a hanging command killed, children and all,
//! when its time is up or the human interrupts.

use std::{path::Path, sync::Arc, time::Duration};

use haest_core::{
    ChildEnv, NoHooks, ToolContext, ToolError, ToolOutput, Tools,
    testing::{ScriptedProvider, text_answer},
    tool::input,
    workspace::{ShutWorkspace, WorkspaceMode},
};
use haest_mods::{Binding, HookSet, Mods, OnError, SystemPromptPayload, ToolBeforePayload};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{Claude, ShellHook, hook_tests::*};

/// The Claude Code hook script under docs/examples/hooks/ that refuses
/// `rm -rf`, run by the tests as a reader would.
const NO_RM_RF_SH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/examples/hooks/no-rm-rf.sh"
);

/// `script` written to `hook.sh` in a fresh directory, and a hook that runs
/// it there.
fn script(body: &str) -> (tempfile::TempDir, ShellHook) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hook.sh"), body).unwrap();
    let hook = ShellHook::new("sh hook.sh").with_cwd(dir.path());
    (dir, hook)
}

/// A `Bash` tool (a probe) behind `hooks`, and a call of it with `command`.
async fn bash(
    hooks: HookSet,
    ui: Arc<TestUi>,
    command: &str,
) -> (Result<ToolOutput, ToolError>, usize) {
    bash_with(hooks, ui, command, &ctx()).await
}

async fn bash_with(
    hooks: HookSet,
    ui: Arc<TestUi>,
    command: &str,
    ctx: &ToolContext,
) -> (Result<ToolOutput, ToolError>, usize) {
    let probe = Probe::new("Bash");
    let mods = Mods::new(env(ui, WorkspaceMode::Real), [hooks]);
    let tools = wrapped(mods, Tools::from_iter([probe.clone()]));
    let out = tools[0]
        .execute(input(json!({ "command": command })).unwrap(), ctx)
        .await;
    (out, probe.ran().len())
}

#[tokio::test]
async fn the_claude_code_script_in_the_docs_refuses_rm_rf_with_a_permission_decision() {
    let hook = ShellHook::new(format!("sh '{NO_RM_RF_SH}'"))
        .with_cwd(env!("CARGO_MANIFEST_DIR"))
        .with_dialect(Claude);
    let hooks = HookSet::new().on_tool_before(Binding::new(hook).with_matcher("Bash"));

    let (refused, ran) = bash(hooks.clone(), TestUi::new(), "rm -rf ~/projects").await;
    let refused = refused.unwrap_err().to_string();
    assert!(refused.contains("rm -rf is not allowed here"), "{refused}");
    assert_eq!(ran, 0);

    let (ok, ran) = bash(hooks, TestUi::new(), "ls").await;
    assert_eq!(ok.unwrap().content, "bash:");
    assert_eq!(ran, 1);
}

#[tokio::test]
async fn a_claude_code_script_that_exits_2_refuses_the_call_with_its_stderr() {
    let (_dir, hook) =
        script("if grep -q 'rm -rf'; then echo 'rm -rf is blocked by policy' >&2; exit 2; fi\n");
    let hooks = HookSet::new().on_tool_before(
        Binding::<ToolBeforePayload>::new(hook.with_dialect(Claude)).with_matcher("Bash"),
    );
    let (refused, ran) = bash(hooks.clone(), TestUi::new(), "rm -rf /").await;
    let refused = refused.unwrap_err().to_string();
    assert!(refused.contains("rm -rf is blocked by policy"), "{refused}");
    assert_eq!(ran, 0);
    assert!(bash(hooks, TestUi::new(), "ls").await.0.is_ok());
}

#[tokio::test]
async fn a_stop_script_keeps_each_turn_going_once_by_stop_hook_active() {
    let (_dir, hook) = script(
        r#"if grep -Eq '"stop_hook_active": *true'; then exit 0; fi
echo '{"decision": "block", "reason": "Run the tests before you stop."}'
"#,
    );
    let mods = Mods::new(
        env(TestUi::new(), WorkspaceMode::Real),
        [HookSet::new().on_turn_end(hook.with_dialect(Claude))],
    );
    let p = ScriptedProvider::new(
        ["Done.", "Tests pass.", "Done again.", "Still pass."].map(text_answer),
    );
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());

    assert_eq!(agent.run("fix it", &mut Vec::new()).await.error, None);
    assert_eq!(p.requests().len(), 2, "sent back once");
    let users: Vec<String> = agent.history().iter().filter_map(user_text).collect();
    assert_eq!(users, ["fix it", "Run the tests before you stop."]);

    // A new turn: the script keeps it going once more.
    assert_eq!(agent.run("again", &mut Vec::new()).await.error, None);
    assert_eq!(p.requests().len(), 4);
}

#[tokio::test]
async fn a_command_that_exits_1_or_prints_no_decision_follows_its_binding_on_error() {
    for (body, says) in [
        ("echo broken >&2; exit 1\n", "the command exited 1: broken"),
        ("echo 'ok!'\n", "the hook's answer is not a decision"),
    ] {
        // tool.before's default is block: a guard that broke refuses the call.
        let (_dir, hook) = script(body);
        let ui = TestUi::new();
        let (refused, ran) = bash(
            HookSet::new().on_tool_before(hook.clone()),
            ui.clone(),
            "ls",
        )
        .await;
        let refused = refused.unwrap_err().to_string();
        assert!(refused.contains(says), "{body}: {refused}");
        assert_eq!(ran, 0, "{body}");
        assert!(ui.errors().iter().any(|e| e.contains(says)), "{body}");

        // `continue`: the human sees it, the call runs.
        let ui = TestUi::new();
        let hooks = HookSet::new().on_tool_before(
            Binding::<ToolBeforePayload>::new(hook).with_on_error(OnError::Continue),
        );
        let (ok, ran) = bash(hooks, ui.clone(), "ls").await;
        assert!(ok.is_ok(), "{body}");
        assert_eq!(ran, 1, "{body}");
        assert!(
            ui.errors().iter().any(|e| e.contains(says)),
            "{body}: {:?}",
            ui.errors()
        );
    }
}

/// A script that writes its pid and its background child's, then waits
/// for the child, which sleeps.
const HANGS_SH: &str = "echo $$ > pid\nsleep 1000 &\necho $! > child\nwait\n";

/// The pids [`HANGS_SH`] wrote in `dir`, once it wrote both.
async fn pids(dir: &Path) -> [String; 2] {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    eventually("the script started its child", || {
        !read("pid").trim().is_empty() && !read("child").trim().is_empty()
    })
    .await;
    [read("pid").trim().into(), read("child").trim().into()]
}

/// Whether `pid` is a live process (a zombie is dead, only not reaped).
fn alive(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("ps runs");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

async fn all_gone(pids: &[String; 2]) {
    for pid in pids {
        eventually(&format!("process {pid} is gone"), || !alive(pid)).await;
    }
}

#[tokio::test]
async fn a_hanging_command_is_killed_with_its_children_when_its_binding_times_out() {
    let (dir, hook) = script(HANGS_SH);
    let hooks = HookSet::new().on_tool_before(
        Binding::<ToolBeforePayload>::new(hook).with_timeout(Duration::from_millis(500)),
    );
    let (refused, ran) = bash(hooks, TestUi::new(), "ls").await;
    let refused = refused.unwrap_err().to_string();
    assert!(refused.contains("did not answer"), "{refused}");
    assert_eq!(ran, 0);
    all_gone(&pids(dir.path()).await).await;
}

#[tokio::test]
async fn a_hanging_command_is_killed_with_its_children_when_the_human_interrupts() {
    let (dir, hook) = script(HANGS_SH);
    let hooks = HookSet::new().on_tool_before(hook);
    let cancel = CancellationToken::new();
    let ctx = ToolContext::new(CWD, ShutWorkspace).with_cancel(cancel.clone());
    let path = dir.path().to_path_buf();
    let interrupt = tokio::spawn(async move {
        let started = pids(&path).await;
        cancel.cancel();
        started
    });
    let (out, ran) = bash_with(hooks, TestUi::new(), "ls", &ctx).await;
    assert!(out.unwrap_err().to_string().contains("Interrupted"));
    assert_eq!(ran, 0);
    all_gone(&interrupt.await.unwrap()).await;
}

#[tokio::test]
async fn a_hook_runs_in_the_conversations_dir_with_only_the_variables_it_was_handed() {
    let (dir, hook) = script(
        r#"printf '"pwd=%s project=%s token=%s home=%s"' "$(pwd -P)" "${CLAUDE_PROJECT_DIR:-none}" "$TOKEN" "${HOME:-none}""#,
    );
    let hook = hook.with_env(ChildEnv::new([("TOKEN", "t0k3n")]));
    let here = dir.path().canonicalize().unwrap();
    let said = |hooks: HookSet| async move {
        Mods::new(env(TestUi::new(), WorkspaceMode::Real), [hooks])
            .system_append()
            .await
    };

    // haest's dialect: the payload is all on stdin.
    let hooks = HookSet::new().on_system_prompt(hook.clone());
    assert_eq!(
        said(hooks).await,
        format!("pwd={} project=none token=t0k3n home=none", here.display())
    );

    // Claude's: the project dir too, plain text as context.
    let claude = hook.with_dialect(Claude);
    let hooks = HookSet::new().on_system_prompt(Binding::<SystemPromptPayload>::new(claude));
    assert_eq!(
        said(hooks).await,
        format!(
            "\"pwd={} project={} token=t0k3n home=none\"",
            here.display(),
            dir.path().display()
        )
    );
}
