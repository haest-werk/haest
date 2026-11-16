//! The files in docs/examples, read by the loader the `haest` command uses: an
//! example that no longer loads, or no longer does what its comments say,
//! fails here.

use haest_core::WorkspaceMode;
use haest_providers::CustomProviderConfig;
use haest_settings::BuiltinTools;

use super::*;
use crate::value::var_parts;

macro_rules! example {
    ($file:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/examples/",
            $file
        ))
    };
}

/// A project inside the example's `trusted_dirs` (`~/projects`).
const PROJECT: &str = "/h/projects/app";

/// haest's environment: what the examples' `${NAME}`s with no default name.
const ENV: &[(&str, &str)] = &[("GITHUB_TOKEN", "ghp"), ("DOCS_MCP_TOKEN", "d0c")];

#[tokio::test]
async fn the_example_files_load_together_and_do_what_they_say() {
    let files = [
        (USER_FILE, example!("config.toml")),
        (
            "/h/projects/app/.haest/config.toml",
            example!("project-config.toml"),
        ),
        ("/h/projects/app/.mcp.json", example!("mcp.json")),
        (
            "/h/.config/haest/providers.json",
            example!("providers.json"),
        ),
    ];
    let r = run(PROJECT, &files, ENV, &[]).await.unwrap();
    assert!(r.notes.is_empty(), "{:?}", r.notes);
    assert!(r.resolved.trusted, "~/projects is trusted");
    assert_eq!(r.config.control_socket, crate::ControlSocket::Watch);
    assert_eq!(r.config.broker_access, crate::BrokerAccess::Watch);
    assert_eq!(r.config.broker_url, None, "commented out: no link");
    assert_eq!(
        r.config.control_dir(),
        std::path::Path::new("/h/.local/share/haest/control")
    );
    let s = &r.config.settings;
    assert_eq!(s.tools, BuiltinTools::ALL);
    assert!(s.subagent_caps().budgets, "the budget parameter is on");
    assert_eq!(s.wind_down_at.share(), 0.8, "winds down at 80%");
    assert_eq!(
        s.workspace,
        WorkspaceMode::Real,
        "the project's file is read"
    );
    let passed: Vec<&str> = r
        .passed_env
        .vars()
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(
        passed,
        ["DATABASE_URL", "GH_TOKEN", "NODE_ENV", "PAGER", "RUST_LOG"]
    );
    assert_eq!(r.passed_env.get("NODE_ENV"), Some("development"));
    let mut servers: Vec<&str> = r.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    servers.sort();
    assert_eq!(servers, ["docs", "github"]);
    assert_eq!(
        r.providers.keys().map(String::as_str).collect::<Vec<_>>(),
        ["gateway", "local"]
    );
    for (name, entry) in &r.providers {
        CustomProviderConfig::from_json(entry).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[tokio::test]
async fn the_example_profiles_read_as_their_comments_say() {
    let files = [(USER_FILE, example!("config.toml"))];
    let profile = |name: &'static str| async move {
        let flags = [("profile", name)];
        run("/w", &files, ENV, &flags)
            .await
            .unwrap()
            .config
            .settings
    };
    let review = profile("review").await;
    assert_eq!(review.tools.words(), ["read", "grep", "glob"]);
    let claude = profile("claude").await;
    assert_eq!(claude.caps.max_turn_usd, 5.0);
    assert_eq!(
        claude.workspace,
        WorkspaceMode::Real,
        "the built-in's keys stay"
    );
}

#[tokio::test]
async fn the_example_shell_variables_each_set_their_setting() {
    let vars: Vec<(&str, &str)> = example!("env.sh")
        .lines()
        .filter_map(|line| line.strip_prefix("export "))
        .map(|line| {
            let (name, value) = line.split_once('=').unwrap();
            (name, value.trim_matches('\''))
        })
        .collect();
    assert!(!vars.is_empty());
    let r = run("/w", &[], &[ENV, &vars].concat(), &[]).await.unwrap();
    assert!(r.notes.is_empty(), "{:?}", r.notes);
    for (name, value) in &vars {
        let def = DEFS.iter().find(|d| d.env() == *name);
        let def = def.unwrap_or_else(|| panic!("{name} is no setting's variable"));
        let source = match def.kind {
            Kind::Vars => r.resolved.var_source(def.key, var_parts(value).0),
            _ => r.resolved.source(def.key),
        };
        assert_eq!(source.to_string(), *name);
    }
}
