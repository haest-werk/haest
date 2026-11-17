//! A host's turns and its end, through the runtime's own methods: what the
//! inbox's gate holds rides in with the human's next turn, every turn is in
//! the session file when it ends, and shut_down leaves nothing running.

use std::{path::Path, sync::Arc, sync::Mutex};

use async_trait::async_trait;
use haest_settings::{BuiltinTools, Settings};
use serde_json::json;

use super::{hardcoded, openrouter_key};
use crate::{
    BuildOptions, RuntimeInputs, ShutDown, World, build,
    haest::{
        ChildEnv, LoopEvent, WorkspaceMode,
        core::fs::{Fs, MemFs},
        http::{HttpClient, HttpError, Request, Response},
        tools::Inbox,
        workspace::RealFs,
    },
    inbox, list_sessions,
    testkit::{Deny, Recorded},
};

/// A model API that refuses every call and keeps each request's body.
#[derive(Default)]
struct Refusing(Mutex<Vec<String>>);

#[async_trait]
impl HttpClient for Refusing {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        let body = String::from_utf8_lossy(&req.body).into_owned();
        self.0.lock().unwrap().push(body);
        let refused = json!({"error": {"message": "refused"}});
        Ok(Response::new(400, "").with_json(&refused))
    }
}

#[tokio::test]
async fn what_the_gate_holds_rides_in_with_the_humans_next_turn() {
    let api = Arc::new(Refusing::default());
    let fs = Arc::new(MemFs::new());
    let mut inputs = RuntimeInputs::new(hardcoded(), World::Fs(fs.clone()));
    inputs.keys = openrouter_key();
    inputs.http = api.clone();
    let ui = Arc::new(Recorded::default());
    let mut rt = build(inputs, ui, Deny, BuildOptions::default())
        .await
        .unwrap();
    // The test's own end of a fresh inbox: what a job or a subagent submits
    // to. One turn in a row with no human message, then the gate holds.
    let (sender, queue) = inbox(&rt.hooks, 1);
    rt.inbox = queue;

    sender.submit("job 1 exited".into());
    let first = rt.next_agent_turn().expect("the first agent turn runs");
    assert!(first.contains("job 1 exited"), "{first}");
    sender.submit("job 2 exited".into());
    assert_eq!(rt.next_agent_turn(), None, "past the cap the gate holds it");

    let mut sink = |_: LoopEvent| {};
    rt.human_turn("what happened?", &mut sink).await;
    let sent = api.0.lock().unwrap().clone();
    let carried = |b: &String| b.contains("what happened?") && b.contains("job 2 exited");
    assert!(sent.iter().any(carried), "{sent:?}");

    // On disk with no flush of the host's.
    let file = Path::new("/data/sessions").join(format!("{}.jsonl", rt.session_id()));
    let saved = fs.read_to_string(&file).await.unwrap();
    assert!(saved.contains("what happened?"));
}

#[tokio::test]
async fn shut_down_leaves_no_job_running_and_the_session_on_disk() {
    let dir = std::env::temp_dir().join(format!("haest-shut-down-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let settings = Settings {
        workspace: WorkspaceMode::Real,
        tools: BuiltinTools {
            bash: true,
            ..BuiltinTools::NONE
        },
        data_dir: dir.join("data"),
        ..Settings::bare(dir.clone(), dir.clone())
    };
    let path = std::env::var("PATH").unwrap_or_default();
    let child_env = ChildEnv::new([("PATH", path)]);
    let mut inputs = RuntimeInputs::new(settings, World::Disk { child_env });
    inputs.keys = openrouter_key();
    let ui = Arc::new(Recorded::default());
    let rt = build(inputs, ui, Deny, BuildOptions::default())
        .await
        .unwrap();
    let jobs = rt
        .jobs
        .clone()
        .expect("Bash in the real workspace leaves jobs");
    let workspace = rt.agent().deps().shared().workspace().clone();
    let ctx = crate::haest::core::tool::ToolContext::new(dir.clone(), workspace);
    jobs.start("sleep 30", "waits", &ctx).unwrap();

    let stopped = rt.shut_down().await;
    assert_eq!(
        stopped,
        ShutDown {
            subagents: 0,
            jobs: 1
        }
    );
    assert!(jobs.running().is_empty(), "the job is gone");
    let saved = list_sessions(&RealFs, &dir.join("data/sessions")).await;
    assert_eq!(saved.len(), 1, "the session is on disk");
    std::fs::remove_dir_all(&dir).unwrap();
}
