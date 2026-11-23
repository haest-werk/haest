//! The runtime reads nothing it was not handed: a conversation built from
//! hardcoded settings over a host's own files finds its instructions there,
//! writes its session there, and has no key it was not given.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::StreamExt;
use haest_core::{
    KeySourceRef, Message, ProviderEvent, ProviderRequest, ShutWorkspace, ToolContext,
    fs::{Fs, MemFs},
};
use haest_providers::http::{HttpClient, HttpError, Request, Response};
use haest_settings::{BuiltinTools, ContextSources, Settings};
use serde_json::json;

use crate::{
    BuildError, BuildOptions, Resume, Runtime, RuntimeInputs, World, build,
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
    let fs = Arc::new(MemFs::from_files([("/w/AGENTS.md", "Answer in haiku.")]));
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

/// A host's own native mod: it adds to the system prompt and the human's text.
struct HouseStyle;

#[async_trait]
impl haest_mods::Mod for HouseStyle {
    fn name(&self) -> &str {
        "house-style"
    }
    async fn system_prompt(
        &self,
        _: &haest_mods::ModEnv,
    ) -> Result<Option<String>, haest_core::error::BoxError> {
        Ok(Some("Use British spelling.".into()))
    }
    async fn prompt_submit(
        &self,
        text: &str,
        _: &haest_mods::ModEnv,
    ) -> Result<Option<String>, haest_core::error::BoxError> {
        Ok(Some(format!("{text} (house style)")))
    }
}

#[tokio::test]
async fn a_hosts_own_mod_runs_in_the_conversation_after_the_built_ins() {
    let inputs = RuntimeInputs::new(hardcoded(), World::files(MemFs::new()))
        .with_keys(openrouter_key())
        .with_mod(HouseStyle);
    assert_eq!(inputs.host_mods().len(), 1);
    let rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();
    let names = rt.mods().names();
    assert_eq!(names, ["approval", "plan", "house-style"]);
    let prompt = rt.agent().deps().system_prompt().system_prompt().await;
    assert!(prompt.contains("Use British spelling."), "{prompt}");
    assert_eq!(rt.mods().rewrite_prompt("hi").await, "hi (house style)");
}

/// The order the mods always ran in: the built-ins, the mod files (the
/// human's, then the project's), the host's own.
#[tokio::test]
async fn the_mod_files_run_after_the_built_ins_and_before_the_hosts_own_mods() {
    let tag =
        |t: &str| format!("export default (haest) => haest.on('prompt.submit', t => t + ' ({t})')");
    let files = MemFs::from_files([
        ("/h/.config/haest/mods/mine.js", tag("mine")),
        ("/w/.haest/mods/repo.js", tag("repo")),
    ]);
    let settings = Settings {
        mods: true,
        project_mods: true,
        ..hardcoded()
    };
    let inputs = RuntimeInputs::new(settings, World::files(files))
        .with_keys(openrouter_key())
        .with_mod(HouseStyle);
    let rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .unwrap();
    let names = rt.mods().names();
    assert_eq!(names, ["approval", "plan", "mod files", "house-style"]);
    let rewritten = rt.mods().rewrite_prompt("hi").await;
    assert_eq!(rewritten, "hi (mine) (repo) (house style)");
}

/// A host's own models: a turn runs on them with no key and no HTTP, and a
/// `/model` switch resolves through them too.
#[tokio::test]
async fn a_conversation_on_the_hosts_own_models_needs_no_key_and_no_network() {
    use haest_core::{
        LoopEvent,
        testing::{ScriptedProvider, text_answer},
    };

    let model = ScriptedProvider::new([text_answer("offline")]);
    let settings = Settings {
        model_spec: "anthropic:claude-opus-5-5".into(),
        ..hardcoded()
    };
    let inputs =
        RuntimeInputs::new(settings, World::files(MemFs::new())).with_models(model.resolver());
    let mut rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
        .await
        .expect("no key is needed: the host's models serve the spec");
    let mut sink = |_: LoopEvent| {};
    let outcome = rt.human_turn("hi", &mut sink).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    let asked = model.requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].model, "claude-opus-5-5");
    rt.set_model_spec("openai:gpt-5.5")
        .expect("the host's models serve it");
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
mod lock;
mod mods;
#[cfg(feature = "session")]
mod session;
mod turns;
