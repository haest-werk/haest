//! The rules: bare by default, each layer over the one before, a project's
//! file read only when trusted, profiles, garbage refused.

mod examples;
mod handed;
mod loader;
mod reference;
mod typed;

use std::{collections::HashMap, path::PathBuf};

use haest_core::{error::report, fs::MemFs};
use haest_settings::{Settings, WordSet};

use super::*;

const HOME: &str = "/h";
const USER_FILE: &str = "/h/.config/haest/config.toml";

fn files(files: &[(&str, &str)]) -> MemFs {
    MemFs::with_files(files.iter().map(|(p, t)| (*p, t.as_bytes())))
}

fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    owned(pairs).into_iter().collect()
}

/// Everything the `haest` command loads in `cwd`, with these files, variables
/// (`HOME` is /h) and flags.
async fn run(
    cwd: &str,
    fs: &[(&str, &str)],
    env: &[(&str, &str)],
    flags: &[(&str, &str)],
) -> Result<Loaded, ConfigError> {
    let env = vars(&[&[("HOME", HOME)], env].concat());
    Loader::standard(Settings::bare(cwd, HOME), &env, &owned(flags))
        .load(&files(fs))
        .await
}

async fn settings(env: &[(&str, &str)], flags: &[(&str, &str)]) -> Settings {
    run("/w", &[], env, flags).await.unwrap().config.settings
}

#[tokio::test]
async fn nothing_set_is_bare_haest_and_says_so() {
    let r = run("/w", &[], &[], &[]).await.unwrap();
    assert_eq!(r.config, Config::bare("/w", HOME));
    assert_eq!(r.resolved.source("tools"), Source::Default);
    assert!(!r.resolved.trusted && r.notes.is_empty());
}

#[tokio::test]
async fn each_layer_beats_the_one_before_and_says_where_a_value_came_from() {
    let file = r#"tools = ["read", "bash"]
model = "a:file"
effort = "low""#;
    let files = [(USER_FILE, file)];
    let r = run(
        "/w",
        &files,
        &[("HAEST_MODEL", "a:env")],
        &[("effort", "high")],
    )
    .await
    .unwrap();
    let s = &r.config.settings;
    assert_eq!(s.tools.words(), ["read", "bash"]);
    assert_eq!(s.model_spec, "a:env");
    assert_eq!(s.effort, Some(haest_core::Effort::High));
    let src = |k: &str| r.resolved.source(k).to_string();
    assert_eq!(src("tools"), USER_FILE);
    assert_eq!(src("model"), "HAEST_MODEL");
    assert_eq!(src("effort"), "--effort");
    assert_eq!(src("max_turn_usd"), "default");
}

#[tokio::test]
async fn a_projects_file_is_read_only_inside_a_trusted_dir() {
    let project = ("/w/app/.haest/config.toml", "tools = \"all\"");
    let untrusted = run("/w/app/src", &[project], &[], &[]).await.unwrap();
    assert!(untrusted.config.settings.tools.is_empty());
    assert!(matches!(
        untrusted.resolved.project_file,
        ProjectFile::Untrusted(_)
    ));
    assert_eq!(untrusted.notes.len(), 1);

    let trust = (USER_FILE, "trusted_dirs = [\"/w\"]");
    let trusted = run("/w/app/src", &[project, trust], &[], &[])
        .await
        .unwrap();
    assert_eq!(
        trusted.config.settings.tools,
        haest_settings::BuiltinTools::ALL
    );
    assert!(trusted.resolved.trusted);
    let by_flag = run("/w/app", &[project], &[], &[("trusted_dirs", ".")])
        .await
        .unwrap();
    assert!(matches!(
        by_flag.resolved.project_file,
        ProjectFile::Read(_)
    ));
}

#[tokio::test]
async fn a_project_can_not_trust_itself_or_say_where_the_config_lives() {
    let trust = (USER_FILE, "trusted_dirs = [\"/w\"]");
    for body in ["trusted_dirs = [\"/\"]", "config_dir = \"/x\""] {
        let project = ("/w/.haest/config.toml", body);
        let err = run("/w", &[project, trust], &[], &[]).await.unwrap_err();
        let in_project = Source::File(project.0.into());
        assert!(
            matches!(&err, ConfigError::Invalid { at, .. } if *at == in_project),
            "{err}"
        );
    }
}

#[tokio::test]
async fn the_claude_profile_turns_on_what_haest_reads_of_claude_code() {
    let r = run("/w", &[], &[], &[("profile", "claude")]).await.unwrap();
    let s = &r.config.settings;
    assert_eq!(s.context.words(), ["agents", "claude"]);
    assert_eq!(s.skills.words(), ["agents", "claude", "plugins"]);
    assert!(s.skill_catalog && s.agents && r.config.mcp);
    assert_eq!(s.tools, haest_settings::BuiltinTools::ALL);
    assert_eq!(s.workspace, haest_core::WorkspaceMode::Real);
    assert_eq!(r.config.env_passthrough, EnvPassthrough::All);
    // Claude Code's Agent tool has no budget: the profile leaves budgets off.
    assert!(!s.subagents.budgets && !s.subagent_caps().budgets);
}

#[tokio::test]
async fn subagent_budgets_are_off_bare_and_on_from_a_file_a_variable_or_a_flag() {
    let bare = run("/w", &[], &[], &[]).await.unwrap();
    assert!(!bare.config.settings.subagent_caps().budgets);
    let file = [(USER_FILE, "subagent_budgets = true")];
    let ways = [
        run("/w", &file, &[], &[]).await,
        run("/w", &[], &[("HAEST_SUBAGENT_BUDGETS", "true")], &[]).await,
        run("/w", &[], &[], &[("subagent_budgets", "true")]).await,
    ];
    for r in ways {
        assert!(r.unwrap().config.settings.subagent_caps().budgets);
    }
}

#[tokio::test]
async fn a_files_profile_table_adds_to_the_built_in_and_env_and_flags_still_win() {
    let file = r#"profile = "claude"
[profiles.claude]
tools = ["read"]
[profiles.review]
tools = ["read", "grep"]"#;
    let files = [(USER_FILE, file)];
    let r = run("/w", &files, &[("HAEST_MCP", "off")], &[])
        .await
        .unwrap();
    assert_eq!(r.config.settings.tools.words(), ["read"]);
    assert!(r.config.settings.agents && !r.config.mcp);
    assert_eq!(r.resolved.source("tools").to_string(), "profile claude");

    let review = run("/w", &files, &[("HAEST_PROFILE", "review")], &[])
        .await
        .unwrap();
    assert_eq!(review.config.settings.tools.words(), ["read", "grep"]);
    assert!(!review.config.settings.agents);

    let err = run("/w", &files, &[], &[("profile", "nope")])
        .await
        .unwrap_err();
    let ConfigError::UnknownProfile { name, known } = err else {
        panic!("{err}");
    };
    // Each profile once: the file's [profiles.claude] adds to the built-in one.
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    assert_eq!((name.as_str(), known), ("nope", vec!["claude", "review"]));
}

type Pairs = &'static [(&'static str, &'static str)];

#[tokio::test]
async fn garbage_is_refused_and_names_where_it_was() {
    // (files, variables, flags, what the error must name)
    let cases: [(Pairs, Pairs, Pairs, &str); 7] = [
        (&[(USER_FILE, "tool = [\"read\"]")], &[], &[], USER_FILE),
        (&[(USER_FILE, "tools = [\"bsh\"]")], &[], &[], "bsh"),
        (
            &[(USER_FILE, "[profiles.x]\nprofile = \"y\"")],
            &[],
            &[],
            "profiles.x",
        ),
        (&[], &[("HAEST_MCP", "maybe")], &[], "HAEST_MCP"),
        (
            &[],
            &[("HAEST_MAX_TURN_USD", "lots")],
            &[],
            "HAEST_MAX_TURN_USD",
        ),
        (&[], &[], &[("workspace", "shut")], "--workspace"),
        (
            &[],
            &[],
            &[("env_passthrough", "some")],
            "--env-passthrough",
        ),
    ];
    for (files, env, flags, names) in cases {
        let err = report(&run("/w", files, env, flags).await.unwrap_err());
        assert!(err.contains(names), "{names}: {err}");
    }
}

/// A count (steps, minutes, tokens) given a fraction must not quietly become a
/// smaller one: `subagent_minutes = 0.5` read as 0 would turn the cap off.
#[tokio::test]
async fn a_number_setting_reads_what_it_was_given_or_refuses_it_by_name() {
    for def in DEFS.iter().filter(|d| d.kind == Kind::Number) {
        match run("/w", &[], &[], &[(def.key, "0.5")]).await {
            Ok(r) => assert_eq!(r.config.get(def.key).as_deref(), Some("0.5"), "{}", def.key),
            Err(e) => assert!(report(&e).contains(&format!("--{}", def.flag())), "{e}"),
        }
    }
    let file = [(USER_FILE, "subagent_minutes = 0.5")];
    let err = report(&run("/w", &file, &[], &[]).await.unwrap_err());
    assert!(
        err.contains(USER_FILE) && err.contains("subagent_minutes"),
        "{err}"
    );
    let err = run("/w", &[], &[("HAEST_MAX_TURN_STEPS", "2.5")], &[])
        .await
        .unwrap_err();
    assert!(report(&err).contains("HAEST_MAX_TURN_STEPS"), "{err}");
    let whole = [(USER_FILE, "subagent_steps = 30.0\nsubagent_usd = 0.5")];
    let s = run("/w", &whole, &[], &[]).await.unwrap().config.settings;
    assert_eq!((s.subagents.steps, s.subagents.usd), (30, 0.5));
}

#[tokio::test]
async fn lists_take_all_none_and_any_order_and_come_out_in_one() {
    let tools =
        |raw: &'static str| async move { settings(&[("HAEST_TOOLS", raw)], &[]).await.tools };
    assert_eq!(tools("bash, read,read").await.words(), ["read", "bash"]);
    assert_eq!(tools("all").await, haest_settings::BuiltinTools::ALL);
    assert!(tools("none").await.is_empty());
    let files = [(USER_FILE, "tools = \"all\"")];
    let flags = [("tools", "")];
    let r = run("/w", &files, &[], &flags).await.unwrap();
    assert!(r.config.settings.tools.is_empty());
}

#[tokio::test]
async fn the_config_dir_comes_from_the_environment_before_any_file_is_read() {
    let files = [("/c/config.toml", "tools = [\"read\"]")];
    let s = run("/w", &files, &[("HAEST_CONFIG_DIR", "/c")], &[])
        .await
        .unwrap()
        .config
        .settings;
    assert_eq!(s.tools.words(), ["read"]);
    assert_eq!(s.config_dir, PathBuf::from("/c"));
    let xdg = settings(&[("XDG_CONFIG_HOME", "/x"), ("XDG_DATA_HOME", "/y")], &[]).await;
    assert_eq!(
        (xdg.config_dir, xdg.data_dir),
        (PathBuf::from("/x/haest"), PathBuf::from("/y/haest"))
    );
}

#[tokio::test]
async fn a_relative_or_home_dir_is_made_absolute_from_where_haest_works() {
    let files = [("/w/c/config.toml", "tools = [\"read\"]")];
    let env = [("HAEST_CONFIG_DIR", "c"), ("HAEST_DATA_DIR", "~/d")];
    let s = run("/w", &files, &env, &[]).await.unwrap().config.settings;
    assert_eq!(s.tools.words(), ["read"]);
    assert_eq!(
        (s.config_dir, s.data_dir),
        (PathBuf::from("/w/c"), PathBuf::from("/h/d"))
    );
}
