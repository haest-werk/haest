//! The `hooks` setting bound: every event through each bridge, the claude
//! dialect naming the session the conversation is on at each event, and an
//! entry's matcher, timeout and `on_error` in its binding.

use std::sync::Arc;

use haest_core::{ChildEnv, workspace::WorkspaceMode};
use haest_mods::{ModEvent, Mods, SessionView};
use serde_json::{Value, json};

use super::read;
use crate::{HookHost, hook_tests::*};

/// A host in `remote`'s directory that may start processes (with `PATH`).
fn host(remote: &Remote) -> HookHost {
    let path = std::env::var("PATH").unwrap_or_default();
    HookHost::new(remote.dir()).with_env(ChildEnv::new([("PATH", path)]))
}

/// `setting` bound for `host`, as the only mod.
fn bound_mods(setting: Value, host: &HookHost, ui: Arc<TestUi>) -> Mods {
    let (hooks, notes) = host.bind(&read(setting).unwrap());
    assert_eq!(notes, Vec::<String>::new());
    Mods::new(env(ui, WorkspaceMode::Real), [hooks.expect("bound")])
}

/// The session `n`, its transcript beside it.
fn session(n: u8) -> SessionView {
    SessionView::new(format!("s-{n}")).with_transcript_path(format!("/sessions/s-{n}.jsonl"))
}

/// Every event of haest-mods binds from the config, through a command and a
/// POST: a new event fails here until bind.rs binds it.
#[tokio::test]
async fn every_event_binds_from_the_config_through_each_bridge() {
    for via in VIAS {
        for event in ModEvent::ALL {
            let remote = Remote::answering(*via, "").await;
            let setting = json!({ event.as_str(): [remote.entry()] });
            let mods = bound_mods(setting, &host(&remote), TestUi::new());
            effect(*event, mods, "[hooks]").await;
            let got = remote.received();
            assert!(!got.is_empty(), "{via:?} {event}: nothing reached the hook");
            assert_eq!(got[0]["event"], json!(event.as_str()), "{via:?} {event}");
        }
    }
}

/// A claude entry names the session the conversation is on at each event,
/// and the new one after a /clear; a haest entry gets it as `session`.
#[tokio::test]
async fn a_claude_entry_names_the_session_as_of_each_event() {
    for via in VIAS {
        let remote = Remote::answering(*via, "").await;
        let mut entry = remote.entry();
        entry["dialect"] = json!("claude");
        let mods = bound_mods(
            json!({ "system.prompt": [entry, remote.entry()] }),
            &host(&remote),
            TestUi::new(),
        );
        mods.env().set_session(session(1));
        mods.system_append().await;
        mods.env().set_session(session(2));
        mods.system_append().await;
        let got = remote.received();
        let (claude, haest): (Vec<&Value>, Vec<&Value>) = got
            .iter()
            .partition(|input| input.get("hook_event_name").is_some());
        let ids: Vec<&Value> = claude.iter().map(|input| &input["session_id"]).collect();
        assert_eq!(ids, [&json!("s-1"), &json!("s-2")], "{via:?}");
        assert_eq!(claude[1]["transcript_path"], json!("/sessions/s-2.jsonl"));
        assert_eq!(claude[0]["hook_event_name"], json!("SessionStart"));
        let named: Vec<&Value> = haest.iter().map(|input| &input["session"]).collect();
        assert_eq!(
            named,
            [
                &json!({ "id": "s-1", "transcriptPath": "/sessions/s-1.jsonl" }),
                &json!({ "id": "s-2", "transcriptPath": "/sessions/s-2.jsonl" }),
            ],
            "{via:?}"
        );
    }
}

/// An entry's matcher, timeout and `on_error` are its binding's.
#[cfg(feature = "shell")]
#[tokio::test]
async fn an_entrys_matcher_timeout_and_on_error_are_its_bindings() {
    use haest_core::Tools;

    let remote = Remote::answering(Via::Shell, r#"{"deny": "not Bash"}"#).await;
    let setting = json!({ "tool.before": [{ "command": "sh hook.sh", "matcher": "Bash" }] });
    let mods = bound_mods(setting, &host(&remote), TestUi::new());
    let (echo, bash) = (Probe::new("Echo"), Probe::new("Bash"));
    let tools = wrapped(mods, Tools::from_iter([echo.clone(), bash.clone()]));
    tools[0].execute(input(json!({})), &ctx()).await.unwrap();
    assert!(remote.received().is_empty(), "Echo is not Bash: not asked");
    let refused = tools[1].execute(input(json!({})), &ctx()).await;
    assert!(refused.unwrap_err().to_string().contains("not Bash"));
    assert_eq!((echo.ran().len(), bash.ran().len()), (1, 0));

    // A command that outlives its 0.2 s: with `continue` the call runs, and
    // the human sees the hook's time ran out.
    let ui = TestUi::new();
    let setting = json!({ "tool.before": [{ "command": "sleep 5", "timeout": 0.2, "on_error": "continue" }] });
    let mods = bound_mods(setting, &host(&remote), ui.clone());
    let tools = wrapped(mods, Tools::from_iter([Probe::new("Bash")]));
    tools[0].execute(input(json!({})), &ctx()).await.unwrap();
    let errors = ui.errors();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("[hooks]") && e.contains("did not answer within 200ms")),
        "{errors:?}"
    );
}

/// A command needs a host that starts processes: one that starts none
/// binds the rest, and says which it left out.
#[tokio::test]
async fn an_entry_the_host_can_not_run_is_left_out_and_said() {
    let setting = read(json!({
        "tool.before": [{ "command": "sh guard.sh" }],
        "turn.end": [{ "command": "sh one.sh" }, { "command": "sh two.sh" }],
    }))
    .unwrap();
    let (hooks, notes) = HookHost::new("/w").bind(&setting);
    if cfg!(feature = "shell") {
        assert!(hooks.is_none());
        assert_eq!(notes.len(), 3);
        assert!(notes[2].starts_with("[[hooks.\"turn.end\"]] #2 is not bound: a command hook needs a host that starts processes"), "{notes:?}");
    } else {
        assert!(
            notes.iter().all(|n| n.contains("`shell` feature")),
            "{notes:?}"
        );
    }
}
