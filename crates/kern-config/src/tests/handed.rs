//! What the loader reads beside the settings: the environment a child gets,
//! the keys, providers.json, routing, and the project's MCP servers and mods
//! only where trusted.

use super::*;

const ENV: Pairs = &[
    ("PATH", "/bin"),
    ("LANG", "C"),
    ("OPENROUTER_API_KEY", "k"),
    ("KERN_TOOLS", "read"),
];

#[tokio::test]
async fn a_child_gets_core_by_default_nothing_with_none_and_everything_with_all() {
    let child = |flags: Pairs| async move { run("/w", &[], ENV, flags).await.unwrap().child_env };
    let core = child(&[]).await;
    assert_eq!(
        (core.get("PATH"), core.get("HOME"), core.get("LANG")),
        (Some("/bin"), Some(HOME), Some("C"))
    );
    assert_eq!(
        core.get("OPENROUTER_API_KEY"),
        None,
        "a key stays with kern"
    );
    assert!(
        child(&[("env_passthrough", "none")])
            .await
            .vars()
            .is_empty()
    );
    let all = child(&[("profile", "claude")]).await;
    assert_eq!(all.get("OPENROUTER_API_KEY"), Some("k"));
    assert_eq!(all.vars().len(), ENV.len() + 1);
}

#[tokio::test]
async fn the_keys_are_the_environment_and_routing_is_parsed_or_noted_with_its_source() {
    let routing = r#"{"global": {"banned": ["novita"], "favorites": []}}"#;
    let mut env = ENV.to_vec();
    env.push(("KERN_PROVIDER_ROUTING", routing));
    let r = run("/w", &[], &env, &[]).await.unwrap();
    assert_eq!(
        r.keys.get("OPENROUTER_API_KEY").map(String::as_str),
        Some("k")
    );
    assert!(r.routing.is_some() && r.notes.is_empty());
    let noted = |r: Loaded, from: &str| {
        assert!(r.routing.is_none());
        assert!(r.notes[0].contains(from), "{:?}", r.notes);
    };
    let bad_var = run("/w", &[], &[("KERN_PROVIDER_ROUTING", "{nope")], &[]);
    noted(bad_var.await.unwrap(), "KERN_PROVIDER_ROUTING");
    let bad_flag = run("/w", &[], &[], &[("provider_routing", "{nope")]);
    noted(bad_flag.await.unwrap(), "--provider-routing");
}

#[tokio::test]
async fn providers_json_comes_from_the_config_dir_and_a_broken_one_stops_the_load() {
    let file = r#"{"local": {"type": "openai", "baseURL": "http://localhost:1/v1"}}"#;
    let path = "/h/.config/kern/providers.json";
    let r = run("/w", &[(path, file)], &[], &[]).await.unwrap();
    assert!(r.providers.contains_key("local"));
    let err = run("/w", &[(path, "{nope")], &[], &[]).await.unwrap_err();
    assert!(err.contains("providers.json"), "{err}");
}

const MCP_JSON: &str = r#"{"mcpServers": {"gh": {"command": "gh-mcp"}}}"#;
const TRUST: (&str, &str) = (USER_FILE, "trusted_dirs = [\"/w\"]");

#[tokio::test]
async fn mcp_servers_are_read_only_with_mcp_on_in_a_trusted_dir() {
    let mcp_file = ("/w/.mcp.json", MCP_JSON);
    let on = [("mcp", "true")];
    let off = run("/w", &[mcp_file, TRUST], &[], &[]).await.unwrap();
    assert!(off.mcp_servers.is_empty() && off.notes.is_empty());
    let untrusted = run("/w", &[mcp_file], &[], &on).await.unwrap();
    assert!(untrusted.mcp_servers.is_empty());
    assert_eq!(untrusted.notes.len(), 1, "{:?}", untrusted.notes);
    let quiet = run("/w", &[], &[], &on).await.unwrap();
    assert!(quiet.notes.is_empty(), "no file, nothing skipped");
    let trusted = run("/w", &[mcp_file, TRUST], &[], &on).await.unwrap();
    let names: Vec<&str> = trusted
        .mcp_servers
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["gh"]);
}

#[tokio::test]
async fn project_mods_stay_on_only_in_a_trusted_dir() {
    let a_mod = ("/w/.kern/mods/m.ts", "export default {}");
    let on = [("project_mods", "true")];
    let untrusted = run("/w", &[a_mod], &[], &on).await.unwrap();
    assert!(!untrusted.config.settings.project_mods);
    assert_eq!(untrusted.notes.len(), 1, "{:?}", untrusted.notes);
    let quiet = run("/w", &[], &[], &on).await.unwrap();
    assert!(!quiet.config.settings.project_mods && quiet.notes.is_empty());
    let trusted = run("/w", &[a_mod, TRUST], &[], &on).await.unwrap();
    assert!(trusted.config.settings.project_mods);
}
