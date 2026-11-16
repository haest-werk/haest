//! OpenRouter's web search server tool: on the wire, and in the folded message.

use haest_core::{
    messages::{WebSearchUse, WebSource},
    provider::{ProviderRequest, ToolSpec, WebSearchConfig, WebSearchEngine},
};
use serde_json::{Value, json};

use super::{
    tests::{chunk, openrouter, or_request, run},
    *,
};

fn search() -> WebSearchConfig {
    WebSearchConfig {
        engine: WebSearchEngine::Exa,
        max_results: 5,
        max_total_results: 15,
    }
}

fn tool_types(body: &Value) -> Vec<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .map(|x| x["type"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn fn_tool_request(web_search: Option<WebSearchConfig>) -> ProviderRequest {
    let mut req = or_request();
    req.tools = vec![ToolSpec {
        name: "Read".into(),
        description: "r".into(),
        parameters: json!({"type": "object"}),
    }];
    req.web_search = web_search;
    req
}

#[test]
fn openrouter_gets_the_server_tool_after_the_function_tools_a_generic_server_never_does() {
    let req = fn_tool_request(Some(search()));
    let body = build_compat_body(&openrouter(), &req);
    assert_eq!(tool_types(&body), ["function", "openrouter:web_search"]);
    assert_eq!(
        body["tools"][1]["parameters"],
        json!({"engine": "exa", "max_results": 5, "max_total_results": 15})
    );
    let local = CompatOptions::new("local", CompatFlavor::Generic, "k");
    assert_eq!(tool_types(&build_compat_body(&local, &req)), ["function"]);
}

#[test]
fn off_sends_no_server_tool() {
    assert_eq!(
        tool_types(&build_compat_body(&openrouter(), &fn_tool_request(None))),
        ["function"]
    );
}

#[tokio::test]
async fn citations_and_the_search_count_land_on_the_message_deduped_by_url() {
    let cite = |url: &str, title: &str| json!({"type": "url_citation", "url_citation": {"url": url, "title": title}});
    let mut usage = chunk(json!({}));
    usage["usage"] = json!({
        "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12,
        "server_tool_use_details": {"web_search_requests": 2},
    });
    let (_, msg) = run(&[
        chunk(json!({"content": "Bun 1.4.4", "annotations": [cite("https://a.test/", "A")]})),
        chunk(json!({"content": ".", "annotations": [cite("https://a.test/", "A"), cite("https://b.test/", "B")]})),
        usage,
    ])
    .await;
    let source = |url: &str, title: &str| WebSource {
        url: url.into(),
        title: Some(title.into()),
    };
    assert_eq!(
        msg.web_search,
        Some(WebSearchUse {
            searches: 2,
            sources: vec![
                source("https://a.test/", "A"),
                source("https://b.test/", "B")
            ]
        })
    );
}

#[tokio::test]
async fn a_reply_with_no_search_carries_no_web_search() {
    let (_, msg) = run(&[chunk(json!({"content": "hi"}))]).await;
    assert_eq!(msg.web_search, None);
}
