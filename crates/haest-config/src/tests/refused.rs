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
        // Any host loads settings: the message points at no command of one.
        let said = err.to_string();
        assert!(
            !said.contains("haest settings") && !said.contains("--help"),
            "{said}"
        );
    }
}

/// A broker URL whose password holds an unescaped `/` reads to every URL
/// parser as another host (`user`, port 1234) with the rest of the
/// password in the path: it is refused where it is given, saying why and
/// how to write it, and showing no part of the password. Percent-encoded,
/// it loads.
#[tokio::test]
async fn a_broker_url_a_parser_misreads_is_refused_where_it_is_given() {
    let url = "wss://user:1234/s3cret@broker.example/agents";
    let quoted = format!("broker_url = \"{url}\"");
    let in_file = [(USER_FILE, quoted.as_str())];
    let cases = [
        (run("/w", &in_file, &[], &[]).await, file(USER_FILE)),
        (
            run("/w", &[], &[("HAEST_BROKER_URL", url)], &[]).await,
            Source::Env("HAEST_BROKER_URL".into()),
        ),
        (
            run("/w", &[], &[], &[("broker_url", url)]).await,
            Source::Flag("broker-url".into()),
        ),
    ];
    for (loaded, at) in cases {
        let err = loaded.expect_err(&at.to_string());
        assert_eq!(refused(&err), Some((&at, "broker_url")), "{err}");
        let said = err.to_string();
        assert!(!said.contains("s3cret") && !said.contains("1234"), "{said}");
        assert!(said.contains("%2F") && said.contains("%40"), "{said}");
    }
    let encoded = "wss://user:1234%2Fs3cret@broker.example/agents";
    let r = run("/w", &[], &[("HAEST_BROKER_URL", encoded)], &[]).await;
    assert_eq!(r.unwrap().config.broker_url.as_deref(), Some(encoded));
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
