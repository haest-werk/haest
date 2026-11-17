//! A tool Code calls winds down with the turn that called Code.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    remedy::Limit,
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
    wind_down::{WindDownWatch, WindingDown},
};
use serde_json::{Value, json};

use super::{code_over, run_in};
use crate::testkit::{MemWorkspace, ctx};
use haest_core::ToolRef;

/// Records whether its call saw the turn winding down.
#[derive(Default)]
struct Sees(Mutex<Vec<bool>>);

#[async_trait]
impl Tool for Sees {
    fn name(&self) -> &str {
        "Sees"
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn description(&self) -> String {
        "x".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn execute(&self, _: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.0
            .lock()
            .unwrap()
            .push(ctx.wind_down().check().is_some());
        Ok(ToolOutput::text("ok"))
    }
}

#[tokio::test]
async fn a_tool_called_from_code_sees_the_callers_wind_down() {
    let sees = Arc::new(Sees::default());
    let tool = code_over(vec![ToolRef::from(sees.clone())], &["Sees"]);
    let code = "await tools.Sees({}); return 1";
    let c = ctx(MemWorkspace::with_files::<&str, &str>([]));
    run_in(&tool, code, json!({}), &c).await;
    let c = c.with_wind_down(WindDownWatch::new(|| {
        Some(WindingDown {
            fact: "the budget is nearly spent: $0.90 of $1 (`max_turn_usd`)".into(),
            limit: Some(Limit::TurnUsd(1.0)),
            remedy: None,
        })
    }));
    run_in(&tool, code, json!({}), &c).await;
    assert_eq!(*sees.0.lock().unwrap(), [false, true]);
}
