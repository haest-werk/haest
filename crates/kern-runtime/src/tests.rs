//! The runtime reads nothing it was not handed: a conversation built from
//! hardcoded settings over a host's own files finds its instructions there,
//! writes its session there, and has no key it was not given.

use std::{path::Path, sync::Arc};

use kern_settings::{BuiltinTools, ContextSources, Settings};

use crate::{
    BuildOptions, RuntimeInputs, World, build,
    kern::fs::{Fs, MemFs},
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

#[tokio::test]
async fn a_key_the_host_hands_in_is_the_key_kern_uses() {
    let fs = Arc::new(MemFs::new());
    let mut inputs = RuntimeInputs::new(hardcoded(), World::Fs(fs));
    inputs.keys = Arc::new(|name: &str| (name == "OPENROUTER_API_KEY").then(|| "k".to_string()));
    let rt = build(
        inputs,
        Arc::new(Recorded::default()),
        Deny,
        BuildOptions::default(),
    )
    .await
    .unwrap();
    assert!(rt.agent.deps().models.resolve(&rt.agent.model_spec).is_ok());
}
