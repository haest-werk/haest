//! The `hooks` setting in a whole conversation: a Claude Code hook script
//! as it is, refusing a Bash call, and told the session the conversation is
//! on; a turn.end command keeping a turn going once; a URL hook POSTed
//! through the runtime's client; an entry the world can't run, said.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_bridges::{
    HookSettings,
    testing::{Respond, Server},
};
use haest_core::{
    ChildEnv, LoopEvent, WorkspaceMode,
    fs::MemFs,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::ToolContext,
};
use haest_mods::{Before, Mod, ModEnv, ModToolCall};
use haest_providers::http::{HttpClient, HttpError, Request, Response};
use haest_settings::{BuiltinTools, Settings};
use serde_json::{Value, json};

use super::hardcoded;
use crate::{
    BuildOptions, Runtime, RuntimeInputs, World, build,
    testkit::{Deny, Recorded},
};

/// The Claude Code PreToolUse script the docs ship.
const NO_RM_RF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/examples/hooks/no-rm-rf.sh"
);

fn setting(hooks: Value) -> HookSettings {
    serde_json::from_value(hooks).expect("a hooks setting")
}

/// The real disk in `dir` (a command may start, with `PATH`), Bash on in
/// the real workspace, sessions under `dir/data`.
fn on_disk(dir: &Path) -> (Settings, World) {
    let settings = Settings {
        workspace: WorkspaceMode::Real,
        tools: BuiltinTools {
            bash: true,
            ..BuiltinTools::NONE
        },
        data_dir: dir.join("data"),
        ..Settings::bare(dir, dir)
    };
    let path = std::env::var("PATH").unwrap_or_default();
    (settings, World::disk(ChildEnv::new([("PATH", path)])))
}

async fn built(inputs: RuntimeInputs) -> Runtime<Recorded> {
    build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap()
}

/// A host mod that writes down each tool call that reaches it.
#[derive(Clone, Default)]
struct Witness(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Mod for Witness {
    fn name(&self) -> &str {
        "witness"
    }
    async fn tool_before(
        &self,
        call: &ModToolCall,
        _: &ToolContext,
        _: &ModEnv,
    ) -> Result<Before, haest_core::error::BoxError> {
        self.0.lock().unwrap().push(call.name.clone());
        Ok(Before::Pass)
    }
}

/// docs/examples/hooks/no-rm-rf.sh, bound from the config in Claude Code's
/// dialect, refuses the call before it runs and before a host's own mod
/// sees it; a Claude hook beside it is told the session the conversation
/// is on, and after a /clear the new one.
#[tokio::test]
async fn a_claude_code_hook_script_from_the_config_refuses_rm_rf() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).unwrap();
    let (settings, world) = on_disk(dir.path());
    let hooks = setting(json!({ "tool.before": [
        { "command": "cat >> seen.jsonl; echo >> seen.jsonl", "dialect": "claude", "matcher": "Bash" },
        { "command": format!("sh {NO_RM_RF}"), "dialect": "claude", "matcher": "Bash" },
    ] }));
    let rm = json!({ "command": format!("rm -rf {}", victim.display()) });
    let model = ScriptedProvider::new([
        tool_answer(&[("call-1", "Bash", rm.clone())]),
        text_answer("refused"),
        tool_answer(&[("call-2", "Bash", rm)]),
        text_answer("refused again"),
    ]);
    let witness = Witness::default();
    let inputs = RuntimeInputs::new(settings, world)
        .with_models(model.resolver())
        .with_hook_settings(hooks)
        .with_mod(witness.clone());
    let mut rt = built(inputs).await;

    let outcome = rt.human_turn("clean up", &mut |_: LoopEvent| {}).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    assert!(victim.exists(), "the command never ran");
    let history = format!("{:?}", rt.agent().history());
    assert!(history.contains("rm -rf is not allowed here"), "{history}");
    assert_eq!(
        *witness.0.lock().unwrap(),
        Vec::<String>::new(),
        "refused before the host's mod"
    );

    let first = (rt.session_id(), session_file(dir.path(), &rt));
    rt.clear().await.unwrap();
    let second = (rt.session_id(), session_file(dir.path(), &rt));
    assert_ne!(first.0, second.0);
    let again = rt.human_turn("again", &mut |_: LoopEvent| {}).await;
    assert_eq!(again.error, None, "{again:?}");
    let seen: Vec<Value> = std::fs::read_to_string(dir.path().join("seen.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let named: Vec<(String, PathBuf)> = seen
        .iter()
        .map(|input| {
            assert_eq!(input["hook_event_name"], "PreToolUse");
            assert_eq!(input["cwd"], json!(dir.path()));
            let id = input["session_id"].as_str().unwrap().to_string();
            (id, input["transcript_path"].as_str().unwrap().into())
        })
        .collect();
    assert_eq!(named, [first, second]);
    assert_eq!(rt.ui().warnings(), Vec::<String>::new());
}

fn session_file(dir: &Path, rt: &Runtime<Recorded>) -> PathBuf {
    dir.join("data/sessions")
        .join(format!("{}.jsonl", rt.session_id()))
}

/// A turn.end command in haest's dialect that answers `continue` the first
/// time: the turn goes on once, with its text, then ends.
#[tokio::test]
async fn a_turn_end_command_from_the_config_keeps_the_turn_going_once() {
    let dir = tempfile::tempdir().unwrap();
    let once = r#"cat > /dev/null; if [ ! -e asked ]; then : > asked; echo '{"continue": "run the tests first"}'; fi"#;
    std::fs::write(dir.path().join("once.sh"), once).unwrap();
    let (settings, world) = on_disk(dir.path());
    let model = ScriptedProvider::new([text_answer("done"), text_answer("tests pass")]);
    let inputs = RuntimeInputs::new(settings, world)
        .with_models(model.resolver())
        .with_hook_settings(setting(
            json!({ "turn.end": [{ "command": "sh once.sh" }] }),
        ));
    let mut rt = built(inputs).await;

    let outcome = rt.human_turn("fix it", &mut |_: LoopEvent| {}).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    let asked = model.requests();
    assert_eq!(asked.len(), 2, "one more model call, then the end");
    assert!(format!("{:?}", asked[1].messages).contains("run the tests first"));
    assert_eq!(rt.ui().warnings(), Vec::<String>::new());
}

/// A URL hook POSTs the event to the server (the default client), in
/// haest's dialect with its header.
#[tokio::test]
async fn a_url_hook_from_the_config_is_posted_the_event() {
    let server = Server::start([Respond::With(200, String::new())]).await;
    let hooks = setting(json!({ "turn.end": [{
        "url": server.url(),
        "headers": { "x-review": "on" },
    }] }));
    let model = ScriptedProvider::new([text_answer("done")]);
    let inputs = RuntimeInputs::new(hardcoded(), World::files(Arc::new(MemFs::new())))
        .with_models(model.resolver())
        .with_hook_settings(hooks);
    let mut rt = built(inputs).await;

    let outcome = rt.human_turn("hi", &mut |_: LoopEvent| {}).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].header("x-review"), Some("on"));
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["event"], "turn.end");
    assert_eq!(rt.ui().warnings(), Vec::<String>::new());
}

/// An HTTP client that answers 200 with no body and keeps each URL it was sent.
#[derive(Default)]
struct Passing(Mutex<Vec<String>>);

#[async_trait]
impl HttpClient for Passing {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        self.0.lock().unwrap().push(req.url.clone());
        Ok(Response::new(200, ""))
    }
}

/// A host's own client carries the POST: no hook leaves the process by a
/// road the host did not hand in.
#[tokio::test]
async fn a_url_hook_goes_through_the_hosts_client() {
    let client = Arc::new(Passing::default());
    let hooks = setting(json!({ "prompt.submit": [{ "url": "https://review.example/prompt" }] }));
    let model = ScriptedProvider::new([text_answer("done")]);
    let inputs = RuntimeInputs::new(hardcoded(), World::files(Arc::new(MemFs::new())))
        .with_models(model.resolver())
        .with_http(client.clone())
        .with_hook_settings(hooks);
    let mut rt = built(inputs).await;
    let _ = rt.human_turn("hi", &mut |_: LoopEvent| {}).await;
    assert_eq!(*client.0.lock().unwrap(), ["https://review.example/prompt"]);
}

/// A world that starts no process binds no command: the rest bind, and the
/// host is told which entry was left out.
#[tokio::test]
async fn a_command_hook_where_no_process_starts_is_left_out_and_said() {
    let client = Arc::new(Passing::default());
    let hooks = setting(json!({
        "tool.before": [{ "command": "sh guard.sh" }],
        "prompt.submit": [{ "url": "https://review.example/prompt" }],
    }));
    let model = ScriptedProvider::new([text_answer("done")]);
    let inputs = RuntimeInputs::new(hardcoded(), World::files(Arc::new(MemFs::new())))
        .with_models(model.resolver())
        .with_http(client.clone())
        .with_hook_settings(hooks);
    let mut rt = built(inputs).await;
    assert_eq!(
        rt.ui().warnings(),
        [
            "[[hooks.\"tool.before\"]] #1 is not bound: a command hook needs a host that starts processes, and this one starts none."
        ]
    );
    let _ = rt.human_turn("hi", &mut |_: LoopEvent| {}).await;
    assert_eq!(client.0.lock().unwrap().len(), 1, "the URL hook binds");
}
