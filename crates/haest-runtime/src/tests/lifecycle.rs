//! A conversation's session over its life. A build that fails has started
//! nothing and left no file; what a host changes while it runs (model,
//! effort, /clear) is in the session file; a /clear that cannot create its
//! file changes nothing.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use haest_core::testing::{ScriptedProvider, text_answer};
use haest_settings::Settings;

use super::{hardcoded, keyed, keyed_with, openrouter_key, try_keyed};
use crate::{
    BuildError, BuildOptions, OnResume, Prefer, Resume, Runtime, RuntimeInputs, SessionFileError,
    World, build,
    haest::{
        ChildEnv, Effort, LoopEvent, Message, ModelError, WorkspaceMode,
        core::fs::{Fs, MemFs},
        core::task_board::NewTask,
        mcp::{McpServerConfig, McpTransport},
    },
    sessions::list_sessions,
    testkit::{Deny, Recorded},
    world::WorkspaceError,
};

const SESSIONS: &str = "/data/sessions";
const OTHER: &str = "openrouter:openai/gpt-5.5";
const GIVEN: &str = "openrouter:z-ai/glm-5";

/// An MCP server at a closed local port: starting it fails at once, with a warning.
fn closed_server() -> McpServerConfig {
    McpServerConfig::new("closed", McpTransport::http("http://127.0.0.1:9/mcp"))
}

/// `settings` over `fs`, the closed server listed: the build's error, and
/// everything the Ui was told.
async fn failed_build(settings: Settings, fs: Arc<MemFs>) -> (BuildError, Vec<String>) {
    let inputs = RuntimeInputs::new(settings, World::files(fs))
        .with_keys(openrouter_key())
        .with_mcp_servers([closed_server()]);
    // The test keeps the UI too, to read what it was told after a failed build.
    let ui = Arc::new(Recorded::default());
    let err = build(inputs, ui.clone(), Deny, BuildOptions::default())
        .await
        .expect_err("the build fails");
    (err, ui.warnings())
}

/// A model the build cannot use for want of `key`: the provider is known,
/// the host handed in no such key.
fn no_key(err: &BuildError, key: &str) -> bool {
    matches!(err, BuildError::Model(ModelError::MissingKey { key: k, .. }) if k == key)
}

/// A file where the sessions dir goes: no session file can be created.
async fn block_sessions(fs: &MemFs) {
    let _ = fs.remove_all(Path::new(SESSIONS)).await;
    fs.create_dir_all(Path::new("/data")).await.unwrap();
    fs.write(Path::new(SESSIONS), b"not a dir").await.unwrap();
}

#[tokio::test]
async fn a_workspace_that_cannot_open_fails_the_build_before_a_server_starts() {
    let fs = Arc::new(MemFs::new());
    let settings = Settings {
        workspace: WorkspaceMode::Real,
        ..hardcoded()
    };
    let (err, heard) = failed_build(settings, fs.clone()).await;
    let needs_disk = matches!(err, BuildError::Workspace(WorkspaceError::RealNeedsDisk));
    assert!(needs_disk, "{err:?}");
    assert_eq!(heard, Vec::<String>::new(), "no server was started");
    let left = list_sessions(fs.as_ref(), Path::new(SESSIONS)).await;
    assert!(left.is_empty(), "no session file is left behind");
}

#[tokio::test]
async fn a_session_that_cannot_be_saved_fails_the_build_before_a_server_starts() {
    let fs = Arc::new(MemFs::new());
    block_sessions(&fs).await;
    let (err, heard) = failed_build(hardcoded(), fs).await;
    let cannot_create = matches!(
        err,
        BuildError::SessionFile(SessionFileError::Create { .. })
    );
    assert!(cannot_create, "{err:?}");
    assert_eq!(heard, Vec::<String>::new(), "no server was started");
}

/// A model the build cannot use (no key for it) fails the build before
/// anything is written: no new, empty session is left for `-r` to pick up.
#[tokio::test]
async fn a_model_the_build_cannot_use_fails_it_and_leaves_no_session_file() {
    let fs = Arc::new(MemFs::new());
    let inputs = RuntimeInputs::new(hardcoded(), World::files(fs.clone()));
    let built = build(inputs, Recorded::default(), Deny, BuildOptions::default()).await;
    let left = list_sessions(fs.as_ref(), Path::new(SESSIONS)).await;
    assert!(left.is_empty(), "no session file is left behind");
    let err = built.expect_err("the build fails");
    assert!(no_key(&err, "OPENROUTER_API_KEY"), "{err:?}");
}

/// A resume onto a model it cannot use fails before it writes anything: the
/// saved session is as it was.
#[tokio::test]
async fn a_resume_onto_a_model_it_cannot_use_leaves_the_session_as_it_was() {
    let fs = Arc::new(MemFs::new());
    let rt = keyed(&fs, Resume::New).await;
    rt.hooks().flush().await;
    let id = rt.session_id();
    let file = Path::new(SESSIONS).join(format!("{id}.jsonl"));
    let saved = fs.read_to_string(&file).await.unwrap();

    let keyless = Settings {
        model_spec: "anthropic:claude-opus-5-5".into(),
        ..hardcoded()
    };
    let opts = BuildOptions {
        resume: Resume::Id(id),
        on_resume: OnResume {
            model: Prefer::Settings,
            ..OnResume::default()
        },
    };
    let built = try_keyed(&fs, keyless, opts).await;
    if let Ok(rt) = &built {
        rt.hooks().flush().await;
    }
    let now = fs.read_to_string(&file).await.unwrap();
    assert_eq!(now, saved, "the session is as it was");
    let err = built.expect_err("the build fails");
    assert!(no_key(&err, "ANTHROPIC_API_KEY"), "{err:?}");
}

#[tokio::test]
async fn a_model_or_effort_switch_outlives_a_resume_and_a_bad_spec_changes_nothing() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, Resume::New).await;
    let before = rt.agent().model_spec().to_string();
    assert!(rt.set_model_spec("nowhere:x").is_err());
    assert_eq!(
        (rt.agent().model_spec(), rt.hooks().model_spec()),
        (before.as_str(), before.clone())
    );

    rt.set_model_spec(OTHER).unwrap();
    rt.set_effort(Some(Effort::High));
    assert_eq!(
        (rt.hooks().model_spec(), rt.hooks().effort()),
        (OTHER.to_string(), Some(Effort::High))
    );
    rt.hooks().flush().await;

    let back = keyed(&fs, Resume::Id(rt.session_id())).await;
    assert_eq!(back.agent().model_spec(), OTHER);
    assert_eq!(back.agent().effort(), Some(Effort::High));
}

/// A resume goes on with the session's own model and effort, unless this run
/// asked for its own on purpose: that one wins, and the next resume keeps it.
#[tokio::test]
async fn a_model_or_effort_given_for_a_resume_wins_over_the_saved_one() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, Resume::New).await;
    rt.set_model_spec(OTHER).unwrap();
    rt.set_effort(Some(Effort::High));
    rt.hooks().flush().await;
    let id = rt.session_id();
    let on =
        |rt: &crate::Runtime<Recorded>| (rt.agent().model_spec().to_string(), rt.agent().effort());

    let plain = keyed(&fs, Resume::Id(id.clone())).await;
    assert_eq!(on(&plain), (OTHER.to_string(), Some(Effort::High)));

    let given = Settings {
        model_spec: GIVEN.into(),
        effort: Some(Effort::Low),
        ..hardcoded()
    };
    let opts = |model, effort| BuildOptions {
        resume: Resume::Id(id.clone()),
        on_resume: OnResume { model, effort },
    };
    let model_only = opts(Prefer::Settings, Prefer::Saved);
    let model_only = keyed_with(&fs, given.clone(), model_only).await;
    assert_eq!(on(&model_only), (GIVEN.to_string(), Some(Effort::High)));
    let both = keyed_with(&fs, given, opts(Prefer::Settings, Prefer::Settings)).await;
    assert_eq!(on(&both), (GIVEN.to_string(), Some(Effort::Low)));
    both.hooks.flush().await;

    let next = keyed(&fs, Resume::Id(id)).await;
    assert_eq!(on(&next), (GIVEN.to_string(), Some(Effort::Low)));
}

/// A conversation over `fs` whose model answers `answer` once, with no key
/// and no network; `resume` as [`BuildOptions`].
async fn answering(fs: &Arc<MemFs>, resume: Resume, answer: &str) -> Runtime<Recorded> {
    let model = ScriptedProvider::new([text_answer(answer)]);
    let inputs =
        RuntimeInputs::new(hardcoded(), World::files(fs.clone())).with_models(model.resolver());
    let opts = BuildOptions {
        resume,
        ..BuildOptions::default()
    };
    build(inputs, Recorded::default(), Deny, opts)
        .await
        .unwrap()
}

/// The text of every user message in `history`.
fn said(history: &[Message]) -> Vec<&str> {
    let users = history.iter().filter_map(|m| match m {
        Message::User(u) => Some(u.content.as_str()),
        _ => None,
    });
    users.collect()
}

/// A session file whose last line a crash tore, resumed: the turn run after
/// the resume is there on the next one.
#[tokio::test]
async fn a_turn_run_after_resuming_a_torn_file_outlives_the_next_resume() {
    let fs = Arc::new(MemFs::new());
    let mut sink = |_: LoopEvent| {};
    let mut rt = answering(&fs, Resume::New, "answer one").await;
    let outcome = rt.human_turn("first question", &mut sink).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    rt.shut_down().await;
    let id = rt.session_id();
    drop(rt);
    // A crash mid-write: a record's first bytes, and no newline.
    let file = Path::new(SESSIONS).join(format!("{id}.jsonl"));
    let torn = br#"{"type":"message","t":1,"message":{"role":"assis"#;
    fs.append(&file, torn).await.unwrap();

    let mut rt = answering(&fs, Resume::Id(id.clone()), "answer two").await;
    let outcome = rt.human_turn("second question", &mut sink).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    rt.shut_down().await;
    drop(rt);

    let back = answering(&fs, Resume::Id(id), "unused").await;
    let history = back.agent().history();
    assert_eq!(said(history), ["first question", "second question"]);
    assert_eq!(history.len(), 4, "a question and its answer, twice");
}

/// A dir of the test's own under the system's temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("haest-{name}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("w")).unwrap();
        Self(dir)
    }

    /// A conversation on the real disk in this dir; `resume` as [`BuildOptions`].
    async fn build(&self, resume: Resume) -> Result<Runtime<Recorded>, BuildError> {
        let settings = Settings {
            workspace: WorkspaceMode::Shut,
            data_dir: self.0.join("data"),
            ..Settings::bare(self.0.join("w"), self.0.clone())
        };
        let model = ScriptedProvider::new([]);
        let inputs = RuntimeInputs::new(settings, World::disk(ChildEnv::default()))
            .with_models(model.resolver());
        let opts = BuildOptions {
            resume,
            ..BuildOptions::default()
        };
        build(inputs, Recorded::default(), Deny, opts).await
    }

    /// [`build`](Self::build) a resume of `id` once no run holds it, waiting
    /// at most 5 s. A process another test forks shares this one's open
    /// files until it starts its program, so a lock just dropped can stay
    /// held that long.
    async fn resume_once_free(&self, id: &str) -> Result<Runtime<Recorded>, BuildError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let built = self.build(Resume::Id(id.to_string())).await;
            if !in_use(&built, id) || Instant::now() > deadline {
                return built;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `built` failed because another run holds the session `id`.
fn in_use(built: &Result<Runtime<Recorded>, BuildError>, id: &str) -> bool {
    let Err(BuildError::SessionFile(SessionFileError::InUse { id: held })) = built else {
        return false;
    };
    held == id
}

/// A session one run writes is not resumed by another while it runs (`haest
/// -r` in a second terminal): that build fails, saying the session is in use.
/// Once the first run is gone, the session resumes.
#[tokio::test]
async fn a_session_another_run_writes_is_refused_until_that_run_is_gone() {
    let dir = Scratch::new("held-session");
    let first = dir.build(Resume::New).await.unwrap();
    let id = first.session_id();

    for resume in [Resume::Id(id.clone()), Resume::Latest] {
        let second = dir.build(resume).await;
        assert!(in_use(&second, &id), "{second:?}");
    }
    first.shut_down().await;
    drop(first);
    let again = dir.resume_once_free(&id).await;
    assert!(again.is_ok(), "{again:?}");
    let held = dir.build(Resume::Id(id.clone())).await;
    assert!(in_use(&held, &id), "the resumed run holds it now: {held:?}");
}

/// A /clear holds the new session and lets the old one go: another run
/// resumes the old one, not the new.
#[tokio::test]
async fn a_clear_holds_the_new_session_and_lets_the_old_one_go() {
    let dir = Scratch::new("clear-held");
    let mut first = dir.build(Resume::New).await.unwrap();
    let old = first.session_id();
    first.clear().await.unwrap();
    let new = first.session_id();

    let on_new = dir.build(Resume::Id(new.clone())).await;
    assert!(in_use(&on_new, &new), "{on_new:?}");
    let on_old = dir.resume_once_free(&old).await;
    assert!(on_old.is_ok(), "{on_old:?}");
}

#[tokio::test]
async fn clear_starts_a_new_session_on_the_same_model_and_the_old_one_still_resumes() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, Resume::New).await;
    rt.set_model_spec(OTHER).unwrap();
    rt.board()
        .create(NewTask {
            subject: "old work".into(),
            ..Default::default()
        })
        .unwrap();
    let old = rt.session_id();

    rt.clear().await.unwrap();
    assert_ne!(rt.session_id(), old);
    assert!(rt.board().list().is_empty());
    rt.hooks().flush().await;

    let fresh = keyed(&fs, Resume::Id(rt.session_id())).await;
    assert_eq!(fresh.agent().model_spec(), OTHER);
    assert!(fresh.board.list().is_empty());
    let resumed = keyed(&fs, Resume::Id(old)).await;
    let kept = resumed.board.list().len();
    assert_eq!(kept, 1, "the old session keeps its tasks");
}

#[tokio::test]
async fn a_clear_that_cannot_create_its_file_changes_nothing() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, Resume::New).await;
    rt.board()
        .create(NewTask {
            subject: "kept".into(),
            ..Default::default()
        })
        .unwrap();
    let id = rt.session_id();
    rt.hooks().flush().await;
    block_sessions(&fs).await;

    let err = rt.clear().await.expect_err("the clear fails");
    assert!(matches!(err, SessionFileError::Create { .. }), "{err:?}");
    assert_eq!(rt.session_id(), id);
    assert_eq!(rt.board().list().len(), 1);
}
