//! The runtime reads nothing it was not handed: a conversation built from
//! hardcoded settings over a host's own files finds its instructions there,
//! writes its session there, and has no key it was not given. What a host
//! changes while it runs (model, effort, /clear) is in the session file.

use std::{path::Path, sync::Arc};

use kern_settings::{BuiltinTools, ContextSources, Settings};

use crate::{
    BuildOptions, Runtime, RuntimeInputs, World, build,
    kern::{
        Effort,
        fs::{Fs, MemFs},
        task_board::NewTask,
    },
    testkit::{Deny, Recorded},
};

fn hardcoded() -> Settings {
    Settings {
        context: ContextSources {
            agents: true,
            ..ContextSources::NONE
        },
        tools: BuiltinTools {
            read: true,
            ..BuiltinTools::NONE
        },
        data_dir: "/data".into(),
        ..Settings::bare("/w", "/h")
    }
}

#[tokio::test]
async fn a_conversation_over_the_hosts_files_reads_and_writes_only_them() {
    let fs = Arc::new(MemFs::with_files([("/w/AGENTS.md", "Answer in haiku.")]));
    let inputs = RuntimeInputs::new(hardcoded(), World::Fs(fs.clone()));
    let ui = Arc::new(Recorded::default());
    let rt = build(inputs, ui.clone(), Deny, BuildOptions::default())
        .await
        .unwrap();

    let prompt = rt.agent.deps().system_prompt.system_prompt().await;
    assert!(
        prompt.contains("Answer in haiku."),
        "AGENTS.md from the host's Fs"
    );
    let tools: Vec<String> = rt
        .agent
        .deps()
        .tools
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(tools, ["Read"]);

    rt.host.flush().await;
    let session = Path::new("/data/sessions").join(format!("{}.jsonl", rt.session_id));
    assert!(fs.exists(&session).await, "the session is in the host's Fs");

    let err = rt
        .agent
        .deps()
        .models
        .resolve(&rt.agent.model_spec)
        .err()
        .unwrap();
    assert!(
        err.contains("OPENROUTER_API_KEY"),
        "no key unless handed: {err}"
    );
    assert_eq!(ui.warnings(), Vec::<String>::new());
}

/// A conversation over `fs` with an OpenRouter key; `resume` as [`BuildOptions`].
async fn keyed(fs: &Arc<MemFs>, resume: Option<String>) -> Runtime<Recorded> {
    let mut inputs = RuntimeInputs::new(hardcoded(), World::Fs(fs.clone()));
    inputs.keys = Arc::new(|name: &str| (name == "OPENROUTER_API_KEY").then(|| "k".to_string()));
    let ui = Arc::new(Recorded::default());
    build(inputs, ui, Deny, BuildOptions { resume })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_key_the_host_hands_in_is_the_key_kern_uses() {
    let rt = keyed(&Arc::new(MemFs::new()), None).await;
    assert!(rt.agent.deps().models.resolve(&rt.agent.model_spec).is_ok());
}

const OTHER: &str = "openrouter:openai/gpt-5.5";

#[tokio::test]
async fn a_model_or_effort_switch_outlives_a_resume_and_a_bad_spec_changes_nothing() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, None).await;
    let before = rt.agent.model_spec.clone();
    assert!(rt.set_model("nowhere:x").is_err());
    assert_eq!(
        (rt.agent.model_spec.as_str(), rt.host.model()),
        (before.as_str(), before.clone())
    );

    rt.set_model(OTHER).unwrap();
    rt.set_effort(Some(Effort::High));
    assert_eq!(
        (rt.host.model(), rt.host.effort()),
        (OTHER.to_string(), Some(Effort::High))
    );
    rt.host.flush().await;

    let back = keyed(&fs, Some(rt.session_id.clone())).await;
    assert_eq!(back.agent.model_spec, OTHER);
    assert_eq!(back.agent.effort, Some(Effort::High));
}

#[tokio::test]
async fn clear_starts_a_new_session_on_the_same_model_and_the_old_one_still_resumes() {
    let fs = Arc::new(MemFs::new());
    let mut rt = keyed(&fs, None).await;
    rt.set_model(OTHER).unwrap();
    rt.board.create(NewTask {
        subject: "old work".into(),
        ..Default::default()
    });
    let old = rt.session_id.clone();

    rt.clear().unwrap();
    assert_ne!(rt.session_id, old);
    assert!(rt.board.list().is_empty());
    rt.host.flush().await;

    let fresh = keyed(&fs, Some(rt.session_id.clone())).await;
    assert_eq!(fresh.agent.model_spec, OTHER);
    assert!(fresh.board.list().is_empty());
    let resumed = keyed(&fs, Some(old)).await;
    assert_eq!(
        resumed.board.list().len(),
        1,
        "the old session keeps its tasks"
    );
}
