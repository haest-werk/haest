use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    split::{advertised_tools, callable_tools},
    *,
};
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

fn all() -> Tools {
    Tools::from_iter([
        ToolRef::new(Named("Read", "Read")),
        ToolRef::new(Named("mcp__x__weather", "current weather for a city")),
        ToolRef::new(Named("mcp__x__stocks", "stock quotes")),
    ])
}

fn names(tools: &[ToolRef]) -> Vec<&str> {
    tools.iter().map(|t| t.name()).collect()
}

/// Stands in for haest-tools' ToolSearch: one marker tool per advertise.
fn marker(_: &[ToolRef]) -> Tools {
    Tools::from_iter([ToolRef::new(Named("ToolSearch", "load deferred tools"))])
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
        names(&advertised_tools(&grow, &all(), marker)),
        ["Read", "ToolSearch", "mcp__x__weather"]
    );
    assert_eq!(
        names(&advertised_tools(&proxy, &all(), marker)),
        ["Read", "ToolSearch"]
    );
    let off = ToolDeferral::new(DeferMode::Off);
    assert_eq!(
        names(&advertised_tools(&off, &all(), marker)),
        names(&all())
    );
}

/// A tool that can look up what it did when a durable run resumes it.
struct LooksUp;

#[async_trait]
impl Tool for LooksUp {
    fn name(&self) -> &str {
        "mcp__x__pay"
    }
    fn description(&self) -> String {
        "pay".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn on_resume(&self) -> crate::tool::OnResume {
        crate::tool::OnResume::Lookup
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("paid"))
    }
}

#[test]
fn a_deferred_tool_keeps_what_it_does_on_resume() {
    let d = ToolDeferral::new(DeferMode::Proxy);
    let tools: Tools = Tools::from_iter([ToolRef::new(LooksUp)]);
    let callable = callable_tools(&d, &tools, marker);
    let pay = callable.iter().find(|t| t.name() == "mcp__x__pay").unwrap();
    assert_eq!(pay.on_resume(), crate::tool::OnResume::Lookup);
}

#[test]
fn nothing_deferrable_adds_no_search_tool() {
    let d = ToolDeferral::new(DeferMode::Proxy);
    let builtins: Tools = all()[..1].iter().cloned().collect();
    assert_eq!(names(&advertised_tools(&d, &builtins, marker)), ["Read"]);
}

#[test]
fn no_mcp_tool_is_kept_loaded_unless_a_host_names_it() {
    let d = ToolDeferral::new(DeferMode::Proxy);
    let tools: Tools = Tools::from_iter([ToolRef::new(Named("mcp__host__send_message", "send"))]);
    assert_eq!(names(&advertised_tools(&d, &tools, marker)), ["ToolSearch"]);
}

#[test]
fn a_host_names_its_own_always_loaded_tools() {
    let d = ToolDeferral::new(DeferMode::Proxy).with_always_loaded(["__weather".to_string()]);
    assert_eq!(
        names(&advertised_tools(&d, &all(), marker)),
        ["Read", "mcp__x__weather", "ToolSearch"]
    );
}

fn host_tools() -> Tools {
    Tools::from_iter([
        ToolRef::new(Named("Read", "Read")),
        ToolRef::new(Named("send_notification", "notify the person")),
        ToolRef::new(Named("search_transcripts", "earlier conversations")),
        ToolRef::new(Named("mcp__x__weather", "current weather for a city")),
    ])
}

#[test]
fn a_host_may_defer_every_tool_but_the_ones_it_always_needs() {
    let d = ToolDeferral::new(DeferMode::Grow).deferring_all_except(["Read".to_string()]);
    assert_eq!(
        names(&advertised_tools(&d, &host_tools(), marker)),
        ["Read", "ToolSearch"]
    );
    d.load(["send_notification"]);
    assert_eq!(
        names(&advertised_tools(&d, &host_tools(), marker)),
        ["Read", "ToolSearch", "send_notification"]
    );
    // The default stays MCP only: a host's own tools are never deferred unasked.
    let mcp = ToolDeferral::new(DeferMode::Grow);
    assert_eq!(
        names(&advertised_tools(&mcp, &host_tools(), marker)),
        [
            "Read",
            "send_notification",
            "search_transcripts",
            "ToolSearch"
        ]
    );
}

#[test]
fn every_tool_is_callable_and_calling_a_deferred_one_loads_it() {
    let d = ToolDeferral::new(DeferMode::Grow).deferring_all_except(["Read".to_string()]);
    let callable = callable_tools(&d, &host_tools(), marker);
    assert_eq!(
        names(&callable),
        [
            "Read",
            "ToolSearch",
            "send_notification",
            "search_transcripts",
            "mcp__x__weather"
        ]
    );
    let notify = callable
        .iter()
        .find(|t| t.name() == "send_notification")
        .unwrap();
    let ctx = ToolContext::new("/", crate::workspace::ShutWorkspace);
    let out = futures::executor::block_on(notify.execute(JsonMap::new(), &ctx)).unwrap();
    assert_eq!(out.content, "send_notification");
    assert_eq!(
        names(&advertised_tools(&d, &host_tools(), marker)),
        ["Read", "ToolSearch", "send_notification"],
        "a direct call loads the tool, as ToolSearch would"
    );
}

#[test]
fn the_tools_a_resumed_conversation_called_count_as_loaded() {
    use crate::{messages::Message, testing::tool_answer};
    let d = ToolDeferral::new(DeferMode::Grow).deferring_all_except(["Read".to_string()]);
    let called = |name: &str| Message::Assistant(tool_answer(&[("c", name, json!({}))]));
    d.load_called(&[
        Message::user("hi"),
        called("search_transcripts"),
        called("Read"),
    ]);
    assert_eq!(
        names(&advertised_tools(&d, &host_tools(), marker)),
        ["Read", "ToolSearch", "search_transcripts"]
    );
}

#[test]
fn every_clone_shares_what_is_loaded() {
    let d = ToolDeferral::new(DeferMode::Grow);
    let search_side = d.clone();
    search_side.load(["mcp__x__weather"]);
    assert!(
        names(&advertised_tools(&d, &all(), marker)).contains(&"mcp__x__weather"),
        "a tool one clone loaded is advertised by the other"
    );
}
