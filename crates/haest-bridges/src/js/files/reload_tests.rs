//! Mod files that change while the conversation runs: what each
//! `mod_reload` does at the next step boundary (before a model call), as the
//! model sees it in its next request.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use haest_core::{
    Message, NoHooks, Tools,
    fs::{Fs, MemFs},
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{JsonMap, Tool, ToolError},
    workspace::WorkspaceMode,
};
use haest_mods::ApprovalDecision;
use serde_json::{Value, json};

use super::*;
use crate::{ModReload, testkit::*};

const FILE: &str = "/home/cfg/mods/m.js";

/// A mod file that adds the tool `name` and hides the host's `hidden`.
fn version(name: &str, hidden: &str) -> String {
    format!(
        "export default function (haest) {{
           haest.tool({{ name: '{name}', description: '{name}', parameters: {{ type: 'object' }},
             execute: (input) => String(input.v ?? '{name} ran').toUpperCase() }})
           haest.disableTool('{hidden}')
         }}"
    )
}

/// The conversation's mods: its mod files, reloaded as `reload` says.
async fn files(fs: &MemFs, reload: ModReload, ui: &Arc<TestUi>) -> Mods {
    let opts = ModFileOptions::new("/home/cfg")
        .with_user_mods(true)
        .with_caps(test_caps())
        .with_reload(reload);
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    Mods::new(
        haest.clone(),
        [ModFiles::load(opts, fs.clone(), haest).await],
    )
}

async fn write(fs: &MemFs, source: &str) {
    fs.write(Path::new(FILE), source.as_bytes()).await.unwrap();
}

/// One step: a model call with `mods` in the loop, beside the host's
/// `Push` and `Pull`. The tools the model was offered.
async fn step(mods: &Mods) -> Vec<String> {
    let p = ScriptedProvider::new([text_answer("ok")]);
    let host = Tools::from_iter([Probe::new("Push"), Probe::new("Pull")]);
    let end = looped(&p, NoHooks, mods, host)
        .run("go", &mut Vec::new())
        .await;
    assert_eq!(end.error, None);
    p.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

#[tokio::test]
async fn off_never_reloads() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::new();
    let mods = files(&fs, ModReload::Off, &ui).await;
    assert_eq!(step(&mods).await, ["Pull", "One"]);

    write(&fs, &version("Two", "Pull")).await;
    assert_eq!(step(&mods).await, ["Pull", "One"]);
    fs.remove(Path::new(FILE)).await.unwrap();
    assert_eq!(step(&mods).await, ["Pull", "One"]);
    assert!(ui.asks().is_empty() && ui.infos().is_empty() && ui.errors().is_empty());
}

#[tokio::test]
async fn auto_loads_a_changed_file_at_the_next_step_and_what_it_hides_follows_it() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::new();
    let mods = files(&fs, ModReload::Auto, &ui).await;
    assert_eq!(step(&mods).await, ["Pull", "One"]);

    write(&fs, &version("Two", "Pull")).await;
    assert_eq!(
        step(&mods).await,
        ["Push", "Two"],
        "Push is back, Pull hidden"
    );
    assert!(ui.asks().is_empty());
    assert_eq!(ui.infos(), [format!("Mod {FILE} reloaded.")]);
}

#[tokio::test]
async fn a_deleted_file_unloads_and_its_tools_leave_the_next_request() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::new();
    let mods = files(&fs, ModReload::Auto, &ui).await;
    assert_eq!(step(&mods).await, ["Pull", "One"]);

    fs.remove(Path::new(FILE)).await.unwrap();
    assert_eq!(step(&mods).await, ["Push", "Pull"]);
    assert_eq!(
        ui.infos(),
        [format!("Mod {FILE} was deleted and is unloaded.")]
    );
    assert_eq!(mods.disabled_tools(), Vec::<String>::new());
}

#[tokio::test]
async fn a_file_that_fails_to_load_keeps_the_version_before_and_is_reported_once() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::new();
    let mods = files(&fs, ModReload::Auto, &ui).await;

    write(&fs, "export default 42").await;
    assert_eq!(step(&mods).await, ["Pull", "One"]);
    assert_eq!(step(&mods).await, ["Pull", "One"]);
    let errors = ui.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains(FILE) && errors[0].contains("the version before runs"),
        "{errors:?}"
    );
    assert!(errors[0].contains("not a function"), "{errors:?}");

    // Fixed, it loads.
    write(&fs, &version("Two", "Pull")).await;
    assert_eq!(step(&mods).await, ["Push", "Two"]);
}

#[tokio::test]
async fn ask_puts_the_new_content_before_the_human_and_a_deny_is_not_asked_again() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::answering([ApprovalDecision::Deny]);
    let mods = files(&fs, ModReload::Ask, &ui).await;
    assert!(ui.asks().is_empty(), "the files there at the start load");

    let two = version("Two", "Pull");
    write(&fs, &two).await;
    assert_eq!(step(&mods).await, ["Pull", "One"], "denied: One still runs");
    let asks = ui.asks();
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].tool_name, FILE);
    assert_eq!(asks[0].input_preview, two, "the human reads all of it");

    assert_eq!(step(&mods).await, ["Pull", "One"]);
    write(&fs, &version("One", "Push")).await;
    assert_eq!(step(&mods).await, ["Pull", "One"]);
    write(&fs, &two).await;
    assert_eq!(step(&mods).await, ["Pull", "One"]);
    assert_eq!(ui.asks().len(), 1, "that content was denied once already");
}

#[tokio::test]
async fn ask_loads_a_new_or_changed_file_on_the_humans_allow() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::answering([ApprovalDecision::Allow, ApprovalDecision::Allow]);
    let mods = files(&fs, ModReload::Ask, &ui).await;

    write(&fs, &version("Two", "Pull")).await;
    assert_eq!(step(&mods).await, ["Push", "Two"]);
    let other = "/home/cfg/mods/n.js";
    fs.write(Path::new(other), version("Three", "Nothing").as_bytes())
        .await
        .unwrap();
    assert_eq!(step(&mods).await, ["Push", "Two", "Three"]);
    let asked: Vec<String> = ui.asks().into_iter().map(|a| a.tool_name).collect();
    assert_eq!(asked, [FILE, other]);
}

#[tokio::test]
async fn ask_asks_before_a_deleted_file_unloads() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::answering([ApprovalDecision::Deny]);
    let mods = files(&fs, ModReload::Ask, &ui).await;

    fs.remove(Path::new(FILE)).await.unwrap();
    assert_eq!(step(&mods).await, ["Pull", "One"], "denied: it still runs");
    assert_eq!(step(&mods).await, ["Pull", "One"]);
    assert_eq!(ui.asks().len(), 1);
    assert!(ui.asks()[0].description.contains("deleted"));
}

#[tokio::test]
async fn an_ask_the_turn_interrupted_is_asked_again_at_the_next_step() {
    let fs = MemFs::from_files([(FILE, version("One", "Push"))]);
    let ui = TestUi::answering([ApprovalDecision::Allow]);
    let mods = files(&fs, ModReload::Ask, &ui).await;
    write(&fs, &version("Two", "Pull")).await;

    let p = ScriptedProvider::new([]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let interrupted = CancellationToken::new();
    interrupted.cancel();
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    let _ = Mod::model_before(&mods, &mut agent, &interrupted, &haest).await;
    assert_eq!(mods.names(), ["mod files"]);
    assert!(step(&mods).await.contains(&"Two".to_string()));
    assert_eq!(ui.asks().len(), 1, "asked once, when it could be answered");
}

/// The agent's own write of a mod file, as a host tool.
struct WritesMod(MemFs, String);

#[async_trait]
impl Tool for WritesMod {
    fn name(&self) -> &str {
        "WriteMod"
    }
    fn description(&self) -> String {
        "writes a mod file".into()
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object" })
    }
    fn kind(&self) -> &str {
        "file.write"
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = Path::new(FILE);
        let written = self.0.write(path, self.1.as_bytes()).await;
        written.map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput::text("written"))
    }
}

#[tokio::test]
async fn a_tool_a_mod_file_written_mid_turn_adds_is_offered_at_the_next_step_and_runs() {
    // The mod directory, with no mod in it yet.
    let fs = MemFs::from_files([("/home/cfg/mods/notes.md", "ideas")]);
    let ui = TestUi::new();
    let mods = files(&fs, ModReload::Auto, &ui).await;
    let p = ScriptedProvider::new([
        tool_answer(&[("t1", "WriteMod", json!({}))]),
        tool_answer(&[("t2", "Shout", json!({ "v": "hi" }))]),
        text_answer("done"),
    ]);
    let writes = WritesMod(fs.clone(), version("Shout", "Nothing"));
    let mut agent = looped(&p, NoHooks, &mods, Tools::new().with(writes));
    let end = agent.run("add a mod", &mut Vec::new()).await;
    assert_eq!(end.error, None);

    let offered = |i: usize| -> Vec<String> {
        p.requests()[i]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect()
    };
    assert_eq!(offered(0), ["WriteMod"]);
    assert_eq!(offered(1), ["WriteMod", "Shout"]);
    let shouted = agent
        .history()
        .iter()
        .any(|m| matches!(m, Message::Tool(r) if !r.is_error && r.content == "HI"));
    assert!(shouted, "{:?}", agent.history());
    assert_eq!(ui.infos(), [format!("Mod {FILE} loaded.")]);
}
