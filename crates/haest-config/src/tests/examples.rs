//! The files in docs/examples, read by the loader the `haest` command uses: an
//! example that no longer loads, or no longer does what its comments say,
//! fails here.

use haest_core::WorkspaceMode;
use haest_settings::BuiltinTools;

use super::*;

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
    // Without the `mcp` feature the one note is that .mcp.json is not read.
    let notes = usize::from(cfg!(not(feature = "mcp")));
    assert_eq!(r.notes.len(), notes, "{:?}", r.notes);
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
    #[cfg(feature = "mcp")]
    {
        let mut servers: Vec<&str> = r.mcp_servers.iter().map(|s| s.name.as_str()).collect();
        servers.sort();
        assert_eq!(servers, ["docs", "github"]);
    }
    let names: Vec<&str> = r.providers.names().collect();
    assert_eq!(names, ["gateway", "local"]);
    for name in names {
        let entry = r.providers.get(name).expect("listed");
        entry.unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}
