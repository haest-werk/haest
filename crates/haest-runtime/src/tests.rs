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

use crate::{
    BuildError, BuildOptions, Resume, Runtime, RuntimeInputs, World, build,
    haest::{
        KeySourceRef, Message, ProviderEvent, ProviderRequest, ShutWorkspace, ToolContext,
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
fn openrouter_key() -> KeySourceRef {
    KeySourceRef::new(|name: &str| (name == "OPENROUTER_API_KEY").then(|| "k".to_string()))
}

#[tokio::test]
async fn a_conversation_over_the_hosts_files_reads_and_writes_only_them() {
    let fs = Arc::new(MemFs::with_files([("/w/AGENTS.md", "Answer in haiku.")]));
    let inputs =
        RuntimeInputs::new(hardcoded(), World::files(fs.clone())).with_keys(openrouter_key());
    let rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();

    let prompt = rt.agent().deps().system_prompt().system_prompt().await;
    assert!(
        prompt.contains("Answer in haiku."),
        "AGENTS.md from the host's Fs"
    );
    let tools: Vec<String> = rt
        .agent()
        .deps()
        .tools()
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(tools, ["Read"]);

    rt.hooks().flush().await;
    let session = Path::new("/data/sessions").join(format!("{}.jsonl", rt.session_id()));
    assert!(fs.exists(&session).await, "the session is in the host's Fs");

    let err = rt
        .agent()
        .deps()
        .shared()
        .models()
        .resolve("anthropic:claude-opus-5-5")
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("ANTHROPIC_API_KEY"),
        "no key unless handed: {err}"
    );
    assert_eq!(rt.ui().warnings(), Vec::<String>::new());
}

/// A conversation over `fs` with an OpenRouter key; `resume` as [`BuildOptions`].
async fn keyed(fs: &Arc<MemFs>, resume: Resume) -> Runtime<Recorded> {
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
) -> Result<Runtime<Recorded>, BuildError> {
    let inputs = RuntimeInputs::new(settings, World::files(fs.clone())).with_keys(openrouter_key());
    build(inputs, Recorded::default(), Deny, opts).await
}

#[tokio::test]
async fn the_wind_down_setting_reaches_the_conversation_and_is_off_unless_set() {
    use haest_core::{
        threshold::{Action, Mark},
        wind_down::WindDown,
    };

    // The marks of the conversation's wind-down thresholds.
    let marks = |rt: &Runtime<Recorded>| -> Vec<Mark> {
        let thresholds = &rt.agent().deps().shared().thresholds();
        let winding = thresholds.iter().filter(|t| t.action() == Action::WindDown);
        winding.map(|t| t.mark()).collect()
    };
    let fs = Arc::new(MemFs::new());
    let off = keyed_with(&fs, hardcoded(), BuildOptions::default()).await;
    assert!(marks(&off).is_empty());
    let on = Settings {
        wind_down_at: WindDown::at(0.8).unwrap(),
        ..hardcoded()
    };
    let on = keyed_with(&fs, on, BuildOptions::default()).await;
    assert_eq!(marks(&on), [Mark::share(0.8); 5]);
}

#[tokio::test]
async fn a_key_the_host_hands_in_is_the_key_haest_uses() {
    let rt = keyed(&Arc::new(MemFs::new()), Resume::New).await;
    assert!(
        rt.agent()
            .deps()
            .shared()
            .models()
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
        Ok(Response::new(400, "").with_json(&json!({"error": {"message": "refused"}})))
    }
}

#[tokio::test]
async fn model_calls_and_generate_image_go_through_the_hosts_client() {
    let gateway = Arc::new(Gateway::default());
    let settings = Settings {
        image: true,
        ..hardcoded()
    };
    let inputs = RuntimeInputs::new(settings, World::files(MemFs::new()))
        .with_keys(openrouter_key())
        .with_http(gateway.clone());
    let rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();

    let model = rt
        .agent()
        .deps()
        .shared()
        .models()
        .resolve(rt.agent().model_spec())
        .unwrap();
    let req = ProviderRequest::new(model.model(), vec![Message::user("hi")]);
    let events: Vec<ProviderEvent> = model.provider().stream(req).collect().await;
    assert!(matches!(events.last(), Some(ProviderEvent::Done(_))));
    assert_eq!(gateway.0.lock().unwrap().len(), 1, "the model call");

    let tools = rt.agent().deps().tools().tools();
    let image = tools.iter().find(|t| t.name() == "GenerateImage").unwrap();
    let ctx = ToolContext::new("/w", ShutWorkspace);
    let input = json!({"prompt": "a lighthouse"});
    let made = image
        .execute(input.as_object().unwrap().clone(), &ctx)
        .await;
    assert!(made.is_err(), "the gateway refused it");
    let sent = gateway.0.lock().unwrap().clone();
    assert!(sent[1].ends_with("/images/models") && sent[2].ends_with("/images"));
}

mod approvals;
mod compaction;
mod deferral;
mod lifecycle;
#[cfg(feature = "session")]
mod session;
mod turns;
