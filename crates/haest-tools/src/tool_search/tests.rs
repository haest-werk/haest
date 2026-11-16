use super::*;
use crate::testkit::{input, shut_ctx as ctx};

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
        let q = input(json!({"query": "select:mcp__x__weather"}));
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
    let stocks = input(json!({"name": "mcp__x__stocks", "input": {"symbol": "AAPL"}}));
    let out = call.execute(stocks, &ctx()).await.unwrap();
    assert_eq!(out.content, "mcp__x__stocks ran with {\"symbol\":\"AAPL\"}");
    let unknown = input(json!({"name": "mcp__x__nope", "input": {}}));
    let err = call.execute(unknown, &ctx()).await;
    assert!(matches!(err, Err(ToolError::Input(_))));
}

#[test]
fn a_hosts_messaging_tools_stay_loaded_so_a_channel_guide_can_name_them() {
    let suffixes = ["__send_message", "__list_conversations"].map(String::from);
    let d = Arc::new(ToolDeferral::new(DeferMode::Proxy).with_always_loaded(suffixes));
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

// A bare name list left gpt-6-luna guessing: it never loaded `open_page` to
// take someone to a page. Each deferred tool gets its first sentence.
#[test]
fn tool_search_says_what_each_deferred_tool_is_for_in_one_short_line() {
    let long = "x".repeat(300);
    let mut tools = all();
    tools.push(Arc::new(Fake(
        "mcp__x__open_page",
        "Take the person's tab to a page. Use it when they ask to go somewhere.\nMore.",
    )));
    tools.push(Arc::new(Fake(
        "mcp__x__long",
        Box::leak(long.into_boxed_str()),
    )));
    tools.push(Arc::new(Fake("mcp__x__bare", "")));
    let d = deferral(DeferMode::Grow);
    let text = find(&advertise(&d, &tools), "ToolSearch").description();
    let lines: Vec<&str> = text.lines().skip_while(|l| !l.starts_with("- ")).collect();
    assert_eq!(lines.len(), 5, "{text}");
    assert_eq!(lines[0], "- mcp__x__weather: current weather for a city");
    assert_eq!(
        lines[2],
        "- mcp__x__open_page: Take the person's tab to a page."
    );
    assert_eq!(lines[3], format!("- mcp__x__long: {}...", "x".repeat(117)));
    assert_eq!(lines[4], "- mcp__x__bare");
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
    let q = input(json!({"query": "no such thing"}));
    assert!(matches!(
        search.execute(q, &ctx()).await,
        Err(ToolError::Input(_))
    ));
}
