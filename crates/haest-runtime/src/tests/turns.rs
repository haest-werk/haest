//! A host's turns and its end, through the runtime's own methods: what the
//! inbox's gate holds rides in with the human's next turn, every turn is in
//! the session file when it ends, a tool with side effects starts with its
//! call in the session file, and shut_down leaves nothing running.

use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use haest_core::{
    ChildEnv, LoopEvent, Message, WorkspaceMode,
    error::BoxError,
    fs::{DirEntry, Fs, FsStat, MemFs},
    session::replay,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::ToolContext,
};
use haest_mods::{Before, Mod, ModEnv, ModToolCall};
use haest_providers::http::{HttpClient, HttpError, Request, Response};
use haest_settings::{BuiltinTools, Settings};
use haest_tools::Inbox;
use haest_workspace::RealFs;
use serde_json::json;

use super::{hardcoded, openrouter_key};
use crate::{
    BuildOptions, RuntimeInputs, ShutDown, World, build,
    inbox::inbox,
    list_sessions,
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
    let inputs = RuntimeInputs::new(hardcoded(), World::files(fs.clone()))
        .with_keys(openrouter_key())
        .with_http(api.clone());
    let mut rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();
    // The test's own end of a fresh inbox: what a job or a subagent submits
    // to. One turn in a row with no human message, then the gate holds.
    let (sender, queue) = inbox(rt.hooks(), 1);
    *rt.inbox_mut() = queue;

    sender.submit("job 1 exited".into());
    let first = rt.next_agent_turn().expect("the first agent turn runs");
    assert!(first.contains("job 1 exited"), "{first}");
    sender.submit("job 2 exited".into());
    assert_eq!(rt.next_agent_turn(), None, "past the cap the gate holds it");

    let mut sink = |_: LoopEvent| {};
    let _ = rt.human_turn("what happened?", &mut sink).await;
    let sent = api.0.lock().unwrap().clone();
    let carried = |b: &String| b.contains("what happened?") && b.contains("job 2 exited");
    assert!(sent.iter().any(carried), "{sent:?}");

    // On disk with no flush of the host's.
    let file = Path::new("/data/sessions").join(format!("{}.jsonl", rt.session_id()));
    let saved = fs.read_to_string(&file).await.unwrap();
    assert!(saved.contains("what happened?"));
}

const SESSIONS: &str = "/data/sessions";

/// A host's files where each append to a session file takes 50 ms (a
/// network or a synced disk).
struct SlowAppends(Arc<MemFs>);

#[async_trait]
impl Fs for SlowAppends {
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.0.read(p).await
    }
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.0.write(p, d).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        if p.starts_with(SESSIONS) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.0.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.0.stat(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.0.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.0.create_dir(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        self.0.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        self.0.remove_all(p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.0.rename(a, b).await
    }
}

/// A tool's name, and the ids of the tool calls the session file held as it
/// started: what a resume after a crash there would see.
type Start = (String, Vec<String>);

/// Keeps a [`Start`] for each tool that starts.
struct Witness {
    fs: Arc<MemFs>,
    seen: Arc<Mutex<Vec<Start>>>,
}

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
    ) -> Result<Before, BoxError> {
        let sessions = self.fs.read_dir(Path::new(SESSIONS)).await?;
        let [file] = sessions.as_slice() else {
            return Err(format!("one session file expected: {sessions:?}").into());
        };
        let text = (self.fs)
            .read_to_string(&Path::new(SESSIONS).join(&file.name))
            .await?;
        let calls = (replay(&text).messages.iter())
            .filter_map(|m| match m {
                Message::Assistant(a) => Some(a.tool_calls().map(|c| c.id.clone())),
                _ => None,
            })
            .flatten()
            .collect();
        self.seen.lock().unwrap().push((call.name.clone(), calls));
        Ok(Before::Pass)
    }
}

/// The answer that asked for a tool with side effects is in the session
/// file before the tool starts, however slow the file's writes: a resume
/// after a crash then knows the call may have run.
#[tokio::test]
async fn a_tool_with_side_effects_starts_with_its_call_in_the_session_file() {
    let files = Arc::new(MemFs::new());
    let settings = Settings {
        tools: BuiltinTools {
            write: true,
            ..BuiltinTools::NONE
        },
        ..hardcoded()
    };
    let write = json!({"file_path": "/w/out.txt", "content": "effect"});
    let model = ScriptedProvider::new([
        tool_answer(&[("call-1", "Write", write)]),
        text_answer("written"),
    ]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let witness = Witness {
        fs: files.clone(),
        seen: seen.clone(),
    };
    let inputs = RuntimeInputs::new(settings, World::files(SlowAppends(files)))
        .with_models(model.resolver())
        .with_mod(witness);
    let mut rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();

    let outcome = rt.human_turn("write it", &mut |_: LoopEvent| {}).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, [("Write".to_string(), vec!["call-1".to_string()])]);
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
    let inputs = RuntimeInputs::new(settings, World::disk(child_env)).with_keys(openrouter_key());
    let rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();
    let jobs = rt
        .jobs()
        .cloned()
        .expect("Bash in the real workspace leaves jobs");
    let workspace = rt.agent().deps().shared().workspace().clone();
    let ctx = ToolContext::new(dir.clone(), workspace);
    jobs.start(haest_tools::NewJob::new("sleep 30"), &ctx)
        .unwrap();

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

/// The `[cache]` setting and the session id reach the wire: an hour on the
/// system prompt of an Anthropic model on OpenRouter, none on the newest
/// message, the session id as its sticky-routing `session_id`.
#[tokio::test]
async fn the_cache_setting_and_the_session_id_reach_the_model_call() {
    let api = Arc::new(Refusing::default());
    let settings = Settings {
        cache: haest_settings::CacheSettings {
            system: haest_settings::BreakpointTtl::OneHour,
            history: haest_settings::BreakpointTtl::Off,
            ..Default::default()
        },
        ..hardcoded()
    };
    let inputs = RuntimeInputs::new(settings, World::files(Arc::new(MemFs::new())))
        .with_keys(openrouter_key())
        .with_http(api.clone());
    let mut rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();
    let _ = rt.human_turn("hi", &mut |_: LoopEvent| {}).await;
    let sent = api.0.lock().unwrap().clone();
    let body: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    assert_eq!(body["session_id"], json!(rt.session_id()));
    let system = &body["messages"][0]["content"][0];
    assert_eq!(
        system["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    let newest = body["messages"].as_array().unwrap().last().unwrap();
    assert!(!newest.to_string().contains("cache_control"), "{newest}");
}
