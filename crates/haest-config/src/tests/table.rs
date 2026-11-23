//! A table setting (`[cache]`): a file's table, a variable's and a flag's
//! TOML, each merged into the layers before it; what it does not take is
//! refused, naming where it was given.

use std::time::Duration;

use haest_settings::{BreakpointTtl, CacheOverride, CacheSettings};

use super::*;

#[tokio::test]
async fn each_layer_of_a_table_merges_into_the_ones_before_it() {
    let user = r#"[cache]
system = "1h"
[cache.models."openrouter:*"]
keep_warm = true"#;
    let project = ("/w/app/.haest/config.toml", "[cache]\nhistory = \"off\"");
    let trust = format!("trusted_dirs = [\"/w\"]\n{user}");
    let r = run(
        "/w/app",
        &[(USER_FILE, &trust), project],
        &[("HAEST_CACHE", "keep_warm_minutes = 30")],
        &[
            ("cache", "{ tools = \"off\" }"),
            ("cache", "models.\"openrouter:*\".system = \"5m\""),
        ],
    )
    .await
    .unwrap();
    let mut openrouter = CacheOverride {
        keep_warm: Some(true),
        system: Some(BreakpointTtl::FiveMinutes),
        ..CacheOverride::default()
    };
    let want = CacheSettings {
        system: BreakpointTtl::OneHour,
        tools: BreakpointTtl::Off,
        history: BreakpointTtl::Off,
        keep_warm_time: Duration::from_secs(1_800),
        models: [("openrouter:*".to_string(), openrouter.clone())].into(),
        ..CacheSettings::default()
    };
    assert_eq!(r.config.settings.cache, want);
    assert_eq!(r.resolved.source("cache").to_string(), "--cache");
    // What `haest settings` shows reads back as the same table.
    let shown = r.config.get("cache").expect("a table set");
    let mut again = Config::bare("/w", HOME);
    again.set("cache", &shown).unwrap();
    assert_eq!(again.settings.cache, want);
    openrouter.system = None;
    assert_ne!(want.models["openrouter:*"], openrouter);
}

#[tokio::test]
async fn an_untouched_table_is_unset_and_reads_as_its_default() {
    let r = run("/w", &[], &[], &[]).await.unwrap();
    assert_eq!(r.config.get("cache"), None);
    assert_eq!(r.config.settings.cache, CacheSettings::default());
}

#[tokio::test]
async fn a_table_it_does_not_take_is_refused_naming_where_it_was() {
    let env = |var: &str| Source::Env(var.into());
    let flag = |name: &str| Source::Flag(name.into());
    let cases: [(Pairs, Pairs, Pairs, Source); 5] = [
        (&[(USER_FILE, "cache = \"1h\"")], &[], &[], file(USER_FILE)),
        (
            &[(USER_FILE, "[cache]\nsytem = \"1h\"")],
            &[],
            &[],
            file(USER_FILE),
        ),
        (&[], &[("HAEST_CACHE", "1h")], &[], env("HAEST_CACHE")),
        (
            &[],
            &[("HAEST_CACHE", "system = \"2h\"")],
            &[],
            env("HAEST_CACHE"),
        ),
        (
            &[],
            &[],
            &[("cache", "{ models = { \"a:*\" = { keep = true } } }")],
            flag("cache"),
        ),
    ];
    for (files, env, flags, at) in cases {
        let err = run("/w", files, env, flags).await.unwrap_err();
        assert!(matches!(&err, ConfigError::Invalid { .. }), "{err:?}");
        assert_eq!(refused(&err), Some((&at, "cache")), "{err}");
    }
}

#[tokio::test]
async fn a_profile_sets_a_table_as_a_file_does() {
    let file = "profile = \"slow\"\n[profiles.slow.cache]\nhistory = \"1h\"";
    let r = run("/w", &[(USER_FILE, file)], &[], &[]).await.unwrap();
    assert_eq!(r.config.settings.cache.history, BreakpointTtl::OneHour);
}
