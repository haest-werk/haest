//! A resumed conversation goes on with the deferred tools it was using: what
//! its history called is loaded, so the model does not have to find it again.

use std::sync::Arc;

use haest_settings::Settings;
use serde_json::json;

use super::{hardcoded, keyed_with};
use crate::{
    BuildOptions, Resume, Runtime,
    haest::{
        Message, core::deferred_tools::DeferMode, core::fs::MemFs, core::session::SessionRecord,
        core::testing::tool_answer,
    },
    testkit::Recorded,
};

/// A mod file that adds a tool named as an MCP tool is: one that defers.
const WEATHER_MOD: &str =
    "export default (haest) => haest.tool({ name: 'mcp__x__weather', execute: () => 'sunny' })";
const WEATHER: &str = "mcp__x__weather";

fn advertised(rt: &Runtime<Recorded>) -> Vec<String> {
    let tools = rt.agent().deps().tools().tools();
    tools.iter().map(|t| t.name().to_string()).collect()
}

#[tokio::test]
async fn a_resumed_conversation_keeps_the_deferred_tools_its_history_called() {
    let fs = Arc::new(MemFs::with_files([(
        "/h/.config/haest/mods/weather.js",
        WEATHER_MOD,
    )]));
    let grow = Settings {
        mods: true,
        deferred_tools: DeferMode::Grow,
        ..hardcoded()
    };
    let rt = keyed_with(&fs, grow.clone(), BuildOptions::default()).await;
    let before = advertised(&rt);
    assert!(
        before.contains(&"ToolSearch".to_string()) && !before.contains(&WEATHER.to_string()),
        "a new conversation finds it through ToolSearch: {before:?}"
    );
    let called = tool_answer(&[("c1", WEATHER, json!({}))]);
    rt.hooks().append(&SessionRecord::Message {
        t: 1,
        message: Message::Assistant(called),
    });
    rt.hooks().flush().await;

    let resume = BuildOptions {
        resume: Resume::Id(rt.session_id()),
        ..BuildOptions::default()
    };
    let back = keyed_with(&fs, grow, resume).await;
    assert!(
        advertised(&back).contains(&WEATHER.to_string()),
        "{:?}",
        advertised(&back)
    );
}
