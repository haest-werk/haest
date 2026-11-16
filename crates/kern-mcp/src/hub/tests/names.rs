//! The names the model sees: only `[A-Za-z0-9_-]`, at most 64 characters, unique
//! across every connected server, and each still calls its own tool on its own server.

use std::collections::BTreeSet;

use super::*;

fn provider_safe(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[tokio::test]
async fn every_name_is_one_providers_accept_unique_across_servers_and_reaches_its_own_tool() {
    let long = "x".repeat(70);
    let (long_1, long_2) = (format!("{long}1"), format!("{long}2"));
    let first = ["a.b", "a_b", "a/b c", "naïve", &long_1, &long_2];
    let hub = McpHub::new();
    let (io, _one) = spawn(TestServer::named("one", &first));
    hub.connect_transport("docs.site", io, &|_| {}).await;
    // Spelled like the first once `.` is rewritten: its tools must not shadow the first's.
    let (io, _two) = spawn(TestServer::named("two", &["a.b"]));
    hub.connect_transport("docs_site", io, &|_| {}).await;
    assert_eq!(hub.failures(), []);

    let tools = hub.tools();
    let names: BTreeSet<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        names.len(),
        tools.len(),
        "two tools share a name: {names:?}"
    );
    for name in &names {
        assert!(provider_safe(name), "a provider rejects {name:?}");
    }

    // Each named tool, called through its own name, answers from its own server.
    let mut reached = Vec::new();
    for t in tools
        .iter()
        .filter(|t| t.description() == "Answers with its name")
    {
        reached.push(t.execute(JsonMap::new(), &ctx()).await.unwrap().content);
    }
    reached.sort();
    let mut want: Vec<String> = first.iter().map(|n| format!("one:{n}")).collect();
    want.push("two:a.b".into());
    want.sort();
    assert_eq!(reached, want);
}
