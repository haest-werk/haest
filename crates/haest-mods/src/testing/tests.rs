//! The kit's own tests: what [`effect`] measures when the mods under test
//! hide the `Echo` it calls.

use async_trait::async_trait;
use haest_core::workspace::WorkspaceMode;
use serde_json::json;

use super::*;
use crate::{api::Mod, events::ModEvent};

/// A mod that hides `Echo`, as `haest.disableTool('Echo')` does.
struct HidesEcho;

#[async_trait]
impl Mod for HidesEcho {
    fn name(&self) -> &str {
        "hides echo"
    }

    fn disabled_tools(&self) -> Vec<String> {
        vec!["Echo".into()]
    }
}

#[tokio::test]
async fn a_tool_event_whose_mods_hide_echo_has_the_effect_of_a_hidden_tool() {
    for event in [ModEvent::ToolBefore, ModEvent::ToolAfter] {
        let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Sandbox), [HidesEcho]);
        assert_eq!(
            effect(event, mods, "hides echo").await,
            json!({ "out": { "hidden": true }, "ran": [] }),
            "{event}"
        );
    }
}
