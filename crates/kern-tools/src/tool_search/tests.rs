use kern_core::workspace::ShutWorkspace;

use super::*;

/// A tool that answers with its name and the input it ran with.
struct Fake(&'static str, &'static str);

#[async_trait]
impl Tool for Fake {
    fn name(&self) -> &str {
        self.0
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn description(&self) -> String {
        self.1.into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let ran = serde_json::to_string(&input).unwrap();
        Ok(ToolOutput::text(format!("{} ran with {ran}", self.0)))
    }
}

fn all() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Fake("Read", "Read")),
        Arc::new(Fake("mcp__x__weather", "current weather for a city")),
        Arc::new(Fake("mcp__x__stocks", "stock quotes")),
    ]
}

fn ctx() -> ToolContext {
    ToolContext::new("/tmp", Arc::new(ShutWorkspace))
}

fn advertise(d: &Arc<ToolDeferral>, all: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    d.advertise(all, deferral_tools(d.clone()))
}

fn names(d: &Arc<ToolDeferral>) -> Vec<String> {
    advertise(d, &all())
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

fn find(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools.iter().find(|t| t.name() == name).cloned().unwrap()
}

fn deferral(mode: DeferMode) -> Arc<ToolDeferral> {
    Arc::new(ToolDeferral::new(mode))
}

#[test]
fn off_advertises_everything_proxy_and_grow_hide_mcp_schemas_behind_tool_search() {
    assert_eq!(
        names(&deferral(DeferMode::Off)),
        ["Read", "mcp__x__weather", "mcp__x__stocks"]
    );
    assert_eq!(
        names(&deferral(DeferMode::Proxy)),
        ["Read", "ToolSearch", "CallTool"]
    );
    assert_eq!(names(&deferral(DeferMode::Grow)), ["Read", "ToolSearch"]);
}

#[tokio::test]
async fn grow_adds_what_tool_search_loads_and_proxy_never_changes_the_list() {
    for mode in [DeferMode::Grow, DeferMode::Proxy] {
        let d = deferral(mode);
        let search = find(&advertise(&d, &all()), "ToolSearch");
        let mut q = JsonMap::new();
        q.insert("query".into(), json!("select:mcp__x__weather"));
        let out = search.execute(q, &ctx()).await.unwrap();
        assert!(out.content.contains("\"name\":\"mcp__x__weather\""));
        let want: &[&str] = if mode == DeferMode::Grow {
            &["Read", "ToolSearch", "mcp__x__weather"]
        } else {
            &["Read", "ToolSearch", "CallTool"]
        };
        assert_eq!(names(&d), want);
    }
}

#[tokio::test]
async fn call_tool_runs_the_named_deferred_tool_with_its_input() {
    let d = deferral(DeferMode::Proxy);
    let call = find(&advertise(&d, &all()), "CallTool");
    let input = json!({"name": "mcp__x__stocks", "input": {"symbol": "AAPL"}});
    let out = call
        .execute(input.as_object().cloned().unwrap(), &ctx())
        .await
        .unwrap();
    assert_eq!(out.content, "mcp__x__stocks ran with {\"symbol\":\"AAPL\"}");
    let unknown = json!({"name": "mcp__x__nope", "input": {}});
    let err = call
        .execute(unknown.as_object().cloned().unwrap(), &ctx())
        .await;
    assert!(matches!(err, Err(ToolError::Input(_))));
}

#[test]
fn the_messaging_tools_stay_loaded_so_a_channel_guide_can_name_them() {
    let d = deferral(DeferMode::Proxy);
    let mut with_host = all();
    with_host.push(Arc::new(Fake("mcp__host__send_message", "send")));
    with_host.push(Arc::new(Fake("mcp__host__list_conversations", "list")));
    let shown: Vec<String> = advertise(&d, &with_host)
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(shown.contains(&"mcp__host__send_message".to_string()));
    assert!(shown.contains(&"mcp__host__list_conversations".to_string()));
    assert!(!shown.contains(&"mcp__x__weather".to_string()));
}

#[tokio::test]
async fn keyword_search_ranks_name_hits_over_description_hits() {
    let pool = &all()[1..];
    let hits = |q: &str| -> Vec<String> {
        search_tools(pool, q)
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    };
    assert_eq!(hits("weather"), ["mcp__x__weather"]);
    assert_eq!(hits("quotes"), ["mcp__x__stocks"]);
    let d = deferral(DeferMode::Grow);
    let search = find(&advertise(&d, &all()), "ToolSearch");
    let mut q = JsonMap::new();
    q.insert("query".into(), json!("no such thing"));
    assert!(matches!(
        search.execute(q, &ctx()).await,
        Err(ToolError::Input(_))
    ));
}
