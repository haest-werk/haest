use async_trait::async_trait;
use serde_json::{Value, json};

use super::*;
use crate::tool::{JsonMap, ToolContext, ToolError, ToolOutput};

struct Named(&'static str, &'static str);

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> String {
        self.1.into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(self.0))
    }
}

fn all() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Named("Read", "Read")),
        Arc::new(Named("mcp__x__weather", "current weather for a city")),
        Arc::new(Named("mcp__x__stocks", "stock quotes")),
    ]
}

fn names(tools: &[Arc<dyn Tool>]) -> Vec<&str> {
    tools.iter().map(|t| t.name()).collect()
}

/// Stands in for kern-tools' ToolSearch: one marker tool per advertise.
fn marker(_: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Named("ToolSearch", "load deferred tools"))]
}

#[test]
fn keyword_search_ranks_name_hits_over_description_hits() {
    let pool = &all()[1..];
    assert_eq!(names(&search_tools(pool, "weather")), ["mcp__x__weather"]);
    assert_eq!(names(&search_tools(pool, "quotes")), ["mcp__x__stocks"]);
    let ranked = search_tools(pool, "stocks city");
    assert_eq!(names(&ranked), ["mcp__x__stocks", "mcp__x__weather"]);
    assert!(search_tools(pool, "nothing-like-it").is_empty());
}

#[test]
fn select_takes_exact_names_in_pool_order() {
    let found = search_tools(&all(), " select:mcp__x__stocks, Read ,nope");
    assert_eq!(names(&found), ["Read", "mcp__x__stocks"]);
}

#[test]
fn only_grow_adds_what_was_loaded() {
    let grow = ToolDeferral::new(DeferMode::Grow);
    let proxy = ToolDeferral::new(DeferMode::Proxy);
    for d in [&grow, &proxy] {
        d.load(["mcp__x__weather"]);
    }
    assert_eq!(
        names(&grow.advertise(&all(), marker)),
        ["Read", "ToolSearch", "mcp__x__weather"]
    );
    assert_eq!(
        names(&proxy.advertise(&all(), marker)),
        ["Read", "ToolSearch"]
    );
    let off = ToolDeferral::new(DeferMode::Off);
    assert_eq!(names(&off.advertise(&all(), marker)), names(&all()));
}

#[test]
fn nothing_deferrable_adds_no_search_tool() {
    let d = ToolDeferral::new(DeferMode::Proxy);
    let builtins = &all()[..1];
    assert_eq!(names(&d.advertise(builtins, marker)), ["Read"]);
}

#[test]
fn a_host_names_its_own_always_loaded_tools() {
    let d = ToolDeferral::new(DeferMode::Proxy).with_always_loaded(["__weather".to_string()]);
    assert_eq!(
        names(&d.advertise(&all(), marker)),
        ["Read", "mcp__x__weather", "ToolSearch"]
    );
}

#[test]
fn modes_parse_their_own_names() {
    for m in DeferMode::ALL {
        assert_eq!(m.as_str().parse::<DeferMode>().unwrap(), m);
    }
    assert!("eager".parse::<DeferMode>().is_err());
}
