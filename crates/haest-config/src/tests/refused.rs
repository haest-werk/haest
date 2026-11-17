//! Garbage is refused, never read as a default: the error names the setting
//! (`key`) and the layer it was given in (`at`), so a host can point at it.

use super::*;

#[tokio::test]
async fn garbage_is_refused_naming_the_setting_and_where_it_was() {
    let env = |var: &str| Source::Env(var.into());
    let flag = |name: &str| Source::Flag(name.into());
    // (files, variables, flags, where, the setting)
    let cases: [(Pairs, Pairs, Pairs, Source, &str); 7] = [
        (
            &[(USER_FILE, "tools = [\"bsh\"]")],
            &[],
            &[],
            file(USER_FILE),
            "tools",
        ),
        (
            &[(USER_FILE, "[profiles.x]\nprofile = \"y\"")],
            &[],
            &[],
            file(USER_FILE),
            "profiles.x.profile",
        ),
        (
            &[(
                USER_FILE,
                "[profiles.x]\ntools = 1\n[profiles.y]\nmodel = \"m\"",
            )],
            &[],
            &[],
            file(USER_FILE),
            "profiles.x.tools",
        ),
        (&[], &[("HAEST_MCP", "maybe")], &[], env("HAEST_MCP"), "mcp"),
        (
            &[],
            &[("HAEST_MAX_TURN_USD", "lots")],
            &[],
            env("HAEST_MAX_TURN_USD"),
            "max_turn_usd",
        ),
        (
            &[],
            &[],
            &[("workspace", "shut")],
            flag("workspace"),
            "workspace",
        ),
        (
            &[],
            &[],
            &[("env_passthrough", "some")],
            flag("env-passthrough"),
            "env_passthrough",
        ),
    ];
    for (files, env, flags, at, key) in cases {
        let err = run("/w", files, env, flags).await.unwrap_err();
        assert!(
            matches!(&err, ConfigError::Invalid { .. }),
            "{key}: {err:?}"
        );
        assert_eq!(refused(&err), Some((&at, key)), "{err}");
    }
}

#[tokio::test]
async fn a_setting_no_one_has_is_unknown_wherever_it_is_given() {
    let in_file = [(USER_FILE, "tool = [\"read\"]")];
    let in_profile = [(USER_FILE, "[profiles.x]\ntool = [\"read\"]")];
    let cases = [
        (run("/w", &in_file, &[], &[]).await, file(USER_FILE), "tool"),
        (
            run("/w", &in_profile, &[], &[]).await,
            file(USER_FILE),
            "profiles.x.tool",
        ),
        (
            run("/w", &[], &[], &[("tool", "read")]).await,
            Source::Flag("tool".into()),
            "tool",
        ),
    ];
    for (loaded, at, key) in cases {
        let err = loaded.unwrap_err();
        let unknown = matches!(&err, ConfigError::UnknownSetting { .. });
        assert!(unknown, "{key}: {err:?}");
        assert_eq!(refused(&err), Some((&at, key)), "{err}");
    }
}

/// A count (steps, tokens) given a fraction must not quietly become a
/// smaller one: `max_turn_steps = 0.5` read as 0 would turn the cap off. A
/// time (`subagent_minutes`) takes the fraction as it is.
#[tokio::test]
async fn a_number_setting_reads_what_it_was_given_or_refuses_it_by_name() {
    for def in DEFS.iter().filter(|d| d.kind == Kind::Number) {
        match run("/w", &[], &[], &[(def.key, "0.5")]).await {
            Ok(r) => assert_eq!(r.config.get(def.key).as_deref(), Some("0.5"), "{}", def.key),
            Err(e) => {
                let flag = Source::Flag(def.flag());
                assert_eq!(refused(&e), Some((&flag, def.key)), "{e}");
            }
        }
    }
    let in_file = [(USER_FILE, "max_turn_steps = 0.5")];
    let err = run("/w", &in_file, &[], &[]).await.unwrap_err();
    let at = file(USER_FILE);
    assert!(
        matches!(&err, ConfigError::Invalid { key, at: a, .. } if key == "max_turn_steps" && *a == at),
        "{err:?}"
    );
    let err = run("/w", &[], &[("HAEST_MAX_TURN_STEPS", "2.5")], &[])
        .await
        .unwrap_err();
    let var = Source::Env("HAEST_MAX_TURN_STEPS".into());
    assert_eq!(refused(&err), Some((&var, "max_turn_steps")), "{err}");
    let whole = [(USER_FILE, "subagent_steps = 30.0\nsubagent_usd = 0.5")];
    let s = run("/w", &whole, &[], &[]).await.unwrap().config.settings;
    assert_eq!((s.subagents.steps, s.subagents.usd), (30, 0.5));
}
