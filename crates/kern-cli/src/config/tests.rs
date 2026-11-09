//! Config rules: flags over variables over defaults, and garbage refused.

use clap::Parser;

use super::*;

fn cfg(argv: &[&str], env: &[(&str, &str)]) -> Result<Config, String> {
    let args = Args::try_parse_from(std::iter::once("kern").chain(argv.iter().copied())).unwrap();
    let env: Env = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from(&args, &env, PathBuf::from("/work"))
}

#[test]
fn a_flag_beats_its_variable_and_the_defaults_fill_the_rest() {
    let c = cfg(
        &["-m", "anthropic:x"],
        &[("KERN_MODEL", "openai:y"), ("HOME", "/h")],
    )
    .unwrap();
    assert_eq!(c.model, "anthropic:x");
    assert_eq!(c.workspace, WorkspaceMode::Sandbox);
    assert_eq!(c.data_dir, PathBuf::from("/h/.local/share/kern"));
    assert_eq!(c.cwd, PathBuf::from("/work"));
    assert_eq!(cfg(&[], &[]).unwrap().model, DEFAULT_MODEL);
}

#[test]
fn garbage_caps_and_levels_are_refused_never_uncapped() {
    assert!(cfg(&[], &[("KERN_MAX_TURN_USD", "lots")]).is_err());
    assert!(cfg(&["-e", "extreme"], &[]).is_err());
    assert!(cfg(&["-w", "shut"], &[]).is_err());
}

#[test]
fn token_caps_follow_the_window_unless_set() {
    let c = cfg(&[], &[]).unwrap();
    let l = c.limits_for(200_000);
    assert_eq!(
        (l.max_request_tokens, l.max_turn_tokens),
        (180_000, 2_000_000)
    );
    let set = cfg(&[], &[("KERN_MAX_REQUEST_TOKENS", "0")]).unwrap();
    assert_eq!(set.limits_for(200_000).max_request_tokens, 0);
    assert_eq!(c.compact_at, 400_000);
}

#[test]
fn resume_without_an_id_means_the_latest_here() {
    assert_eq!(cfg(&["-r"], &[]).unwrap().resume, Some(String::new()));
    assert_eq!(
        cfg(&["--resume", "abc"], &[]).unwrap().resume,
        Some("abc".into())
    );
    assert_eq!(cfg(&[], &[]).unwrap().resume, None);
}

#[test]
fn features_default_as_the_ts_kern_and_refuse_garbage() {
    let f = cfg(&[], &[]).unwrap().features;
    assert_eq!(
        f.web_search
            .map(|w| (w.engine, w.max_results, w.max_total_results)),
        Some((kern::provider::WebSearchEngine::Exa, 5, 15))
    );
    assert_eq!(f.code, Some(vec![]));
    assert!(f.approvals && !f.project_mods);

    let f = cfg(
        &[],
        &[
            ("KERN_WEB_SEARCH", "off"),
            ("KERN_CODE_TOOLS", " Bash , mcp__gh__* ,"),
            ("KERN_APPROVALS", "off"),
            ("KERN_PROJECT_MODS", "1"),
        ],
    )
    .unwrap()
    .features;
    assert_eq!(f.web_search, None);
    assert_eq!(f.code, Some(vec!["Bash".into(), "mcp__gh__*".into()]));
    assert!(!f.approvals && f.project_mods);
    assert_eq!(
        cfg(&[], &[("KERN_CODE", "off")]).unwrap().features.code,
        None
    );

    for (key, raw) in [
        ("KERN_WEB_SEARCH", "google"),
        ("KERN_CODE", "yes"),
        ("KERN_APPROVALS", "0"),
        ("KERN_PROJECT_MODS", "on"),
    ] {
        assert!(cfg(&[], &[(key, raw)]).is_err(), "{key}={raw}");
    }
}
