//! The runtime reads nothing it was not handed: a conversation built from
//! hardcoded settings over a host's own files finds its instructions there,
//! writes its session there, and has no key it was not given.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::StreamExt;
use haest_settings::{BuiltinTools, ContextSources, Settings};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    BuildOptions, Runtime, RuntimeInputs, World, build,
    haest::{
        KeySource, Message, ProviderEvent, ProviderRequest, ShutWorkspace, ToolContext,
        core::fs::{Fs, MemFs},
        http::{HttpClient, HttpError, Request, Response},
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

/// The host's keys: an OpenRouter key (the default model's), nothing else.
fn openrouter_key() -> Arc<dyn KeySource> {
    Arc::new(|name: &str| (name == "OPENROUTER_API_KEY").then(|| "k".to_string()))
}

#[tokio::test]
async fn a_conversation_over_the_hosts_files_reads_and_writes_only_them() {
    let fs = Arc::new(MemFs::with_files([("/w/AGENTS.md", "Answer in haiku.")]));
    let mut inputs = RuntimeInputs::new(hardcoded(), World::Fs(fs.clone()));
    inputs.keys = openrouter_key();
    let ui = Arc::new(Recorded::default());
    let rt = build(inputs, ui.clone(), Deny, BuildOptions::default())
        .await
        .unwrap();

    let prompt = rt.agent().deps().system_prompt.system_prompt().await;
    assert!(
        prompt.contains("Answer in haiku."),
        "AGENTS.md from the host's Fs"
    );
    let tools: Vec<String> = rt
        .agent()
        .deps()
        .tools
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(tools, ["Read"]);

    rt.hooks.flush().await;
    let session = Path::new("/data/sessions").join(format!("{}.jsonl", rt.session_id()));
    assert!(fs.exists(&session).await, "the session is in the host's Fs");

    let err = rt
        .agent()
        .deps()
        .models
        .resolve("anthropic:claude-opus-5-5")
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("ANTHROPIC_API_KEY"),
        "no key unless handed: {err}"
    );
    assert_eq!(ui.warnings(), Vec::<String>::new());
}

/// A conversation over `fs` with an OpenRouter key; `resume` as [`BuildOptions`].
async fn keyed(fs: &Arc<MemFs>, resume: Option<String>) -> Runtime<Recorded> {
    let opts = BuildOptions {
        resume,
        ..BuildOptions::default()
    };
    keyed_with(fs, hardcoded(), opts).await
}

/// The same on `settings`, built with `opts`.
async fn keyed_with(fs: &Arc<MemFs>, settings: Settings, opts: BuildOptions) -> Runtime<Recorded> {
    try_keyed(fs, settings, opts).await.unwrap()
}

/// [`keyed_with`]'s build, which may fail.
async fn try_keyed(
    fs: &Arc<MemFs>,
    settings: Settings,
    opts: BuildOptions,
) -> anyhow::Result<Runtime<Recorded>> {
    let mut inputs = RuntimeInputs::new(settings, World::Fs(fs.clone()));
    inputs.keys = openrouter_key();
    let ui = Arc::new(Recorded::default());
    build(inputs, ui, Deny, opts).await
}

#[tokio::test]
async fn a_key_the_host_hands_in_is_the_key_haest_uses() {
    let rt = keyed(&Arc::new(MemFs::new()), None).await;
    assert!(
        rt.agent()
            .deps()
            .models
            .resolve(rt.agent().model_spec())
            .is_ok()
    );
}

/// A host's gateway: the URL of every request it is sent, each refused.
#[derive(Default)]
struct Gateway(Mutex<Vec<String>>);

#[async_trait]
impl HttpClient for Gateway {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        self.0.lock().unwrap().push(req.url);
        Ok(Response::json(
            400,
            &json!({"error": {"message": "refused"}}),
        ))
    }
}

#[tokio::test]
async fn model_calls_and_generate_image_go_through_the_hosts_client() {
    let gateway = Arc::new(Gateway::default());
    let settings = Settings {
        image: true,
        ..hardcoded()
    };
    let mut inputs = RuntimeInputs::new(settings, World::Fs(Arc::new(MemFs::new())));
    inputs.keys = openrouter_key();
    inputs.http = gateway.clone();
    let rt = build(
        inputs,
        Arc::new(Recorded::default()),
        Deny,
        BuildOptions::default(),
    )
    .await
    .unwrap();

    let model = rt
        .agent()
        .deps()
        .models
        .resolve(rt.agent().model_spec())
        .unwrap();
    let req = ProviderRequest {
        model: model.model,
        system: String::new(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        cancel: CancellationToken::new(),
        effort: None,
        max_tokens: None,
        web_search: None,
    };
    let events: Vec<ProviderEvent> = model.provider.stream(req).collect().await;
    assert!(matches!(events.last(), Some(ProviderEvent::Done(_))));
    assert_eq!(gateway.0.lock().unwrap().len(), 1, "the model call");

    let tools = rt.agent().deps().tools.tools();
    let image = tools.iter().find(|t| t.name() == "GenerateImage").unwrap();
    let ctx = ToolContext::new("/w", Arc::new(ShutWorkspace));
    let input = json!({"prompt": "a lighthouse"});
    let made = image
        .execute(input.as_object().unwrap().clone(), &ctx)
        .await;
    assert!(made.is_err(), "the gateway refused it");
    let sent = gateway.0.lock().unwrap().clone();
    assert!(sent[1].ends_with("/images/models") && sent[2].ends_with("/images"));
}

mod compaction;
mod lifecycle;
