//! A conversation's session over its life. A build that fails has started
//! nothing and left no file; what a host changes while it runs (model,
//! effort, /clear) is in the session file; a /clear that cannot create its
//! file changes nothing.

use std::{path::Path, sync::Arc};

use haest_settings::Settings;

use super::{hardcoded, keyed, keyed_with, openrouter_key, try_keyed};
use crate::{
    BuildError, BuildOptions, Resume, RuntimeInputs, SessionError, World, build,
    haest::{
        Effort, ModelError, WorkspaceMode,
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
    let mut inputs = RuntimeInputs::new(settings, World::Fs(fs));
    inputs.keys = openrouter_key();
    inputs.mcp_servers = vec![closed_server()];
    let ui = Arc::new(Recorded::default());
    let err = build(inputs, ui.clone(), Deny, BuildOptions::default())
        .await
        .err()
        .expect("the build fails");
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
    let cannot_create = matches!(err, BuildError::Session(SessionError::Create { .. }));
    assert!(cannot_create, "{err:?}");
    assert_eq!(heard, Vec::<String>::new(), "no server was started");
}

/// A model the build cannot use (no key for it) fails the build before
/// anything is written: no new, empty session is left for `-r` to pick up.
#[tokio::test]
async fn a_model_the_build_cannot_use_fails_it_and_leaves_no_session_file() {
    let fs = Arc::new(MemFs::new());
    let inputs = RuntimeInputs::new(hardcoded(), World::Fs(fs.clone()));
    let ui = Arc::new(Recorded::default());
    let built = build(inputs, ui, Deny, BuildOptions::default()).await;
    let left = list_sessions(fs.as_ref(), Path::new(SESSIONS)).await;
    assert!(left.is_empty(), "no session file is left behind");
    let err = built.err().expect("the build fails");
    assert!(no_key(&err, "OPENROUTER_API_KEY"), "{err:?}");
}

/// A resume onto a model it cannot use fails before it writes anything: the
/// saved session is as it was.
#[tokio::test]
async fn a_resume_onto_a_model_it_cannot_use_leaves_the_session_as_it_was() {
    let fs = Arc::new(MemFs::new());
    let rt = keyed(&fs, Resume::New).await;
    rt.hooks.flush().await;
    let id = rt.session_id();
    let file = Path::new(SESSIONS).join(format!("{id}.jsonl"));
    let saved = fs.read_to_string(&file).await.unwrap();

    let keyless = Settings {
        model_spec: "anthropic:claude-opus-5-5".into(),
        ..hardcoded()
    };
    let opts = BuildOptions {
        resume: Resume::Id(id),
        model_spec_given: true,
        ..BuildOptions::default()
    };
    let built = try_keyed(&fs, keyless, opts).await;
    if let Ok(rt) = &built {
        rt.hooks.flush().await;
    }
    let now = fs.read_to_string(&file).await.unwrap();
    assert_eq!(now, saved, "the session is as it was");
    let err = built.err().expect("the build fails");
    assert!(no_key(&err, "ANTHROPIC_API_KEY"), "{err:?}");
}

#[tokio::test]
async fn a_model_or_effort_switch_outlives_a_resume_and_a_bad_spec_changes_nothing() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, Resume::New).await;
    let before = rt.agent().model_spec().to_string();
    assert!(rt.set_model_spec("nowhere:x").is_err());
    assert_eq!(
        (rt.agent().model_spec(), rt.hooks.model_spec()),
        (before.as_str(), before.clone())
    );

    rt.set_model_spec(OTHER).unwrap();
    rt.set_effort(Some(Effort::High));
    assert_eq!(
        (rt.hooks.model_spec(), rt.hooks.effort()),
        (OTHER.to_string(), Some(Effort::High))
    );
    rt.hooks.flush().await;

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
    rt.hooks.flush().await;
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
    let opts = |model_spec_given, effort_given| BuildOptions {
        resume: Resume::Id(id.clone()),
        model_spec_given,
        effort_given,
    };
    let model_only = keyed_with(&fs, given.clone(), opts(true, false)).await;
    assert_eq!(on(&model_only), (GIVEN.to_string(), Some(Effort::High)));
    let both = keyed_with(&fs, given, opts(true, true)).await;
    assert_eq!(on(&both), (GIVEN.to_string(), Some(Effort::Low)));
    both.hooks.flush().await;

    let next = keyed(&fs, Resume::Id(id)).await;
    assert_eq!(on(&next), (GIVEN.to_string(), Some(Effort::Low)));
}

#[tokio::test]
async fn clear_starts_a_new_session_on_the_same_model_and_the_old_one_still_resumes() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, Resume::New).await;
    rt.set_model_spec(OTHER).unwrap();
    rt.board
        .create(NewTask {
            subject: "old work".into(),
            ..Default::default()
        })
        .unwrap();
    let old = rt.session_id();

    rt.clear().await.unwrap();
    assert_ne!(rt.session_id(), old);
    assert!(rt.board.list().is_empty());
    rt.hooks.flush().await;

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
    rt.board
        .create(NewTask {
            subject: "kept".into(),
            ..Default::default()
        })
        .unwrap();
    let id = rt.session_id();
    rt.hooks.flush().await;
    block_sessions(&fs).await;

    let err = rt.clear().await.expect_err("the clear fails");
    assert!(matches!(err, SessionError::Create { .. }), "{err:?}");
    assert_eq!(rt.session_id(), id);
    assert_eq!(rt.board.list().len(), 1);
}
