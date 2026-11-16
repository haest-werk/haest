//! What the loader reads beside the settings: the environment a child gets,
//! the keys, providers.json, routing, and the project's MCP servers and mods
//! only where trusted.

use super::*;

const ENV: Pairs = &[
    ("PATH", "/bin"),
    ("LANG", "C"),
    ("OPENROUTER_API_KEY", "k"),
    ("HAEST_TOOLS", "read"),
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
        "a key stays with haest"
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
    env.push(("HAEST_PROVIDER_ROUTING", routing));
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
    let bad_var = run("/w", &[], &[("HAEST_PROVIDER_ROUTING", "{nope")], &[]);
    noted(bad_var.await.unwrap(), "HAEST_PROVIDER_ROUTING");
    let bad_flag = run("/w", &[], &[], &[("provider_routing", "{nope")]);
    noted(bad_flag.await.unwrap(), "--provider-routing");
}

#[tokio::test]
async fn providers_json_comes_from_the_config_dir_and_a_broken_one_stops_the_load() {
    let file = r#"{"local": {"type": "openai", "baseURL": "http://localhost:1/v1"}}"#;
    let path = "/h/.config/haest/providers.json";
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

/// `env` adds variable by variable, each layer over the one before; a value's
/// `${NAME}`s come from haest's environment.
#[tokio::test]
async fn env_vars_layer_name_by_name_and_each_says_where_it_came_from() {
    let user = r#"trusted_dirs = ["/w"]
[env]
GH_TOKEN = "${GITHUB_TOKEN}"
A = "file"
B = "${MISSING:-fallback}"
PORT = 8080
SPACED = "  x  ""#;
    let project = (
        "/w/.haest/config.toml",
        "[env]\nA = \"project\"\nPROJ = \"1\"",
    );
    let env = [
        ("GITHUB_TOKEN", "ghp"),
        ("OWN", "mine"),
        ("HAEST_ENV", "V=var"),
    ];
    let flags = [("env", "A=flag"), ("env", "OWN"), ("env", "GONE")];
    let r = run("/w", &[(USER_FILE, user), project], &env, &flags)
        .await
        .unwrap();
    let passed: Vec<(&str, &str)> = r
        .passed_env
        .vars()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        passed,
        [
            ("A", "flag"),
            ("B", "fallback"),
            ("GH_TOKEN", "ghp"),
            ("OWN", "mine"),
            ("PORT", "8080"),
            ("PROJ", "1"),
            ("SPACED", "  x  "),
            ("V", "var"),
        ]
    );
    assert_eq!(r.notes.len(), 1, "{:?}", r.notes);
    assert!(r.notes[0].contains("GONE"), "{:?}", r.notes);
    let src = |name: &str| r.resolved.var_source("env", name).to_string();
    assert_eq!(
        [src("A"), src("GH_TOKEN"), src("PROJ"), src("V")],
        ["--env", USER_FILE, "/w/.haest/config.toml", "HAEST_ENV"]
    );
    assert_eq!(r.child_env.get("GH_TOKEN"), None, "env is handed beside it");

    let untrusted = run("/w", &[project], &[], &[]).await.unwrap();
    assert!(untrusted.passed_env.vars().is_empty());
}

/// `env` as a whole came from the last layer that passed a variable, as any
/// other setting's source is the last layer that set it.
#[tokio::test]
async fn env_as_a_whole_came_from_the_last_layer_that_passed_a_variable() {
    let from_flag = run("/w", &[], &[("HAEST_ENV", "A=1")], &[("env", "B=2")]).await;
    let r = from_flag.unwrap();
    assert_eq!(r.resolved.source("env"), Source::Flag("env".into()));
    let from_var = run("/w", &[], &[("HAEST_ENV", "A=1")], &[]).await.unwrap();
    assert_eq!(
        from_var.resolved.source("env"),
        Source::Env("HAEST_ENV".into())
    );
    let none = run("/w", &[], &[], &[]).await.unwrap();
    assert_eq!(none.resolved.source("env"), Source::Default);
}

#[tokio::test]
async fn a_bad_variable_is_refused_by_its_name_and_never_shows_its_value() {
    let cases: [(Pairs, Pairs, &str); 4] = [
        (&[(USER_FILE, "env = \"A=secret\"")], &[], "[env]"),
        (
            &[(USER_FILE, "[env]\n\"bad-name\" = \"secret\"")],
            &[],
            "bad-name",
        ),
        (&[(USER_FILE, "[env]\n\"A=B\" = \"secret\"")], &[], "A=B"),
        (&[], &[("env", "1X=secret")], "--env"),
    ];
    for (files, flags, names) in cases {
        let err = run("/w", files, &[], flags).await.unwrap_err();
        assert!(err.contains(names), "{names}: {err}");
        assert!(!err.contains("secret"), "{err}");
    }
}

#[tokio::test]
async fn project_mods_stay_on_only_in_a_trusted_dir() {
    let a_mod = ("/w/.haest/mods/m.ts", "export default {}");
    let on = [("project_mods", "true")];
    let untrusted = run("/w", &[a_mod], &[], &on).await.unwrap();
    assert!(!untrusted.config.settings.project_mods);
    assert_eq!(untrusted.notes.len(), 1, "{:?}", untrusted.notes);
    let quiet = run("/w", &[], &[], &on).await.unwrap();
    assert!(!quiet.config.settings.project_mods && quiet.notes.is_empty());
    let trusted = run("/w", &[a_mod, TRUST], &[], &on).await.unwrap();
    assert!(trusted.config.settings.project_mods);
}
