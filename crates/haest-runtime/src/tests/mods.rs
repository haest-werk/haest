//! A mod file's loop events and hidden tools in a built conversation: its
//! `turn.end` keeps a turn going, the tools it hides are gone from main's
//! list (the agent tools too, which no mod hook wraps), and a compaction it
//! asks for is saved, so a resume picks it up.

use std::sync::Arc;

use haest_core::{
    LoopEvent, Message,
    fs::MemFs,
    testing::{ScriptedProvider, text_answer},
};
use haest_settings::{BuiltinTools, Settings};

use crate::{
    BuildOptions, Resume, Runtime, RuntimeInputs, World, build,
    testkit::{Deny, Recorded},
};

/// Where `Settings::bare("/w", "/h")` finds the human's mods.
const MODS: &str = "/h/.config/haest/mods";

/// The human's mods on, Read and the agent tools offered, a session file
/// under `/data`.
fn with_mods() -> Settings {
    Settings {
        mods: true,
        agents: true,
        tools: BuiltinTools {
            read: true,
            ..BuiltinTools::NONE
        },
        model_spec: "anthropic:claude-opus-5-5".into(),
        data_dir: "/data".into(),
        ..Settings::bare("/w", "/h")
    }
}

/// A conversation over `fs` on `model`, resuming as `resume` says.
async fn built(fs: &Arc<MemFs>, model: &ScriptedProvider, resume: Resume) -> Runtime<Recorded> {
    let inputs =
        RuntimeInputs::new(with_mods(), World::files(fs.clone())).with_models(model.resolver());
    let opts = BuildOptions {
        resume,
        ..BuildOptions::default()
    };
    build(inputs, Recorded::default(), Deny, opts)
        .await
        .unwrap()
}

/// A MemFs holding one mod file, `name`, with `source`.
fn one_mod(name: &str, source: &str) -> Arc<MemFs> {
    Arc::new(MemFs::from_files([(format!("{MODS}/{name}"), source)]))
}

#[tokio::test]
async fn a_mod_files_turn_end_keeps_a_turn_going_in_a_built_conversation() {
    let until = "export default (haest) => haest.on('turn.end', v =>
      v.historyLength < 4 ? { continue: 'go on' } : undefined)";
    let fs = one_mod("until.js", until);
    let model = ScriptedProvider::new([text_answer("one"), text_answer("two")]);
    let mut rt = built(&fs, &model, Resume::New).await;

    let end = rt.human_turn("hi", &mut |_: LoopEvent| {}).await;

    assert_eq!(end.error, None, "{:?}", rt.ui().warnings());
    assert_eq!(model.requests().len(), 2);
    let went_on = &rt.agent().history()[2];
    assert!(matches!(went_on, Message::User(u) if u.content == "go on"));
}

#[tokio::test]
async fn the_tools_a_mod_file_hides_are_gone_from_mains_list_agent_tools_too() {
    let names = |rt: &Runtime<Recorded>| -> Vec<String> {
        let tools = rt.agent().deps().tools().tools();
        tools.iter().map(|t| t.name().to_string()).collect()
    };
    let model = ScriptedProvider::new([]);
    let all = names(
        &built(
            &one_mod("none.js", "export default () => {}"),
            &model,
            Resume::New,
        )
        .await,
    );
    let agent_tool = all
        .iter()
        .find(|n| *n != "Read")
        .expect("an agent tool")
        .clone();

    let hides = format!(
        "export default (haest) => {{ haest.disableTool('Read'); haest.disableTool('{agent_tool}') }}"
    );
    let rt = built(&one_mod("hides.js", &hides), &model, Resume::New).await;
    let left = names(&rt);
    assert!(
        !left.contains(&"Read".to_string()) && !left.contains(&agent_tool),
        "{left:?}"
    );
    assert_eq!(left.len(), all.len() - 2, "{all:?} -> {left:?}");
}

#[tokio::test]
async fn a_compaction_a_mod_file_asks_for_is_saved_and_a_resume_picks_it_up() {
    let compacts = "export default (haest) => haest.on('turn.end', () => ({ compact: true }))";
    let fs = one_mod("compacts.js", compacts);
    let model = ScriptedProvider::new([text_answer("done"), text_answer("SUMMARY")]);
    let mut rt = built(&fs, &model, Resume::New).await;
    let end = rt.human_turn("hi", &mut |_: LoopEvent| {}).await;
    assert_eq!(end.error, None, "{:?}", rt.ui().warnings());
    let compacted = rt.agent().history().to_vec();
    assert!(matches!(&compacted[0], Message::User(u) if u.content.contains("SUMMARY")));
    rt.hooks().flush().await;
    let id = rt.session_id();
    drop(rt);

    let back = built(&fs, &model, Resume::Id(id)).await;
    assert_eq!(back.agent().history(), compacted);
}
