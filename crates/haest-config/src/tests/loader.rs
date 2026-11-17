//! The builder: a host picks the layers and their order, and reads nothing it
//! did not add.

use super::*;

const NAMED: &str = "/etc/host.toml";

fn bare() -> Loader {
    Loader::new(Settings::bare("/w", HOME))
}

#[tokio::test]
async fn a_layer_added_later_wins_whatever_kind_it_is() {
    let fs = files(&[(NAMED, "model = \"a:file\"")]);
    let env = vars(&[("HAEST_MODEL", "a:env")]);
    let file_last = bare().env(&env).file(NAMED).load(&fs).await.unwrap();
    assert_eq!(file_last.config.settings.model_spec, "a:file");
    assert_eq!(file_last.resolved.source("model").to_string(), NAMED);
    let env_last = bare().file(NAMED).env(&env).load(&fs).await.unwrap();
    assert_eq!(env_last.config.settings.model_spec, "a:env");
}

#[tokio::test]
async fn a_layer_not_added_is_not_read() {
    let fs = files(&[(USER_FILE, "tools = \"all\""), ("/w/.mcp.json", "{}")]);
    let env = vars(&[("HAEST_TOOLS", "all"), ("OPENROUTER_API_KEY", "k")]);
    let only_flags = bare().flags([("model", "a:flag")]).load(&fs).await.unwrap();
    assert!(only_flags.config.settings.tools.is_empty());
    assert_eq!(only_flags.config.settings.model_spec, "a:flag");
    assert!(only_flags.keys.is_empty() && only_flags.child_env.vars().is_empty());

    let nothing = bare().xdg_dirs(&env).load(&fs).await.unwrap();
    assert_eq!(nothing.config, Config::bare("/w", HOME));
}

#[tokio::test]
async fn a_file_the_host_names_must_be_there_and_is_read_as_the_users_own() {
    let err = bare().file(NAMED).load(&MemFs::new()).await.unwrap_err();
    let missing = matches!(&err, ConfigError::NoSuchFile { path } if path.as_os_str() == NAMED);
    assert!(missing, "{err}");
    let fs = files(&[(NAMED, "trusted_dirs = [\"/w\"]")]);
    assert!(bare().file(NAMED).load(&fs).await.unwrap().resolved.trusted);
}

#[tokio::test]
async fn the_hosts_own_defaults_are_the_bottom_layer() {
    let defaults = Settings {
        tools: haest_settings::BuiltinTools::ALL,
        ..Settings::bare("/w", HOME)
    };
    let none = MemFs::new();
    let kept = Loader::new(defaults.clone());
    let kept = kept.user_file().load(&none).await.unwrap();
    assert_eq!(
        kept.config.settings.tools,
        haest_settings::BuiltinTools::ALL
    );
    assert_eq!(kept.resolved.source("tools"), Source::Default);
    let fs = files(&[(USER_FILE, "tools = [\"read\"]")]);
    let over = Loader::new(defaults).user_file().load(&fs).await.unwrap();
    assert_eq!(over.config.settings.tools.words(), ["read"]);
}

#[tokio::test]
async fn a_profile_named_with_no_profile_layer_is_a_note_not_a_silent_nothing() {
    let r = bare()
        .flags([("profile", "claude")])
        .load(&MemFs::new())
        .await
        .unwrap();
    assert!(r.config.settings.tools.is_empty());
    assert_eq!(r.notes.len(), 1, "{:?}", r.notes);
    assert!(r.notes[0].contains("claude"), "{:?}", r.notes);
}

#[tokio::test]
async fn the_xdg_dirs_say_they_came_from_the_variable() {
    let env = vars(&[("XDG_DATA_HOME", "/y")]);
    let r = bare().xdg_dirs(&env).load(&MemFs::new()).await.unwrap();
    assert_eq!(r.config.settings.data_dir, PathBuf::from("/y/haest"));
    assert_eq!(r.resolved.source("data_dir").to_string(), "XDG_DATA_HOME");
}

#[test]
fn a_built_in_profile_sets_only_what_a_profile_may() {
    for (name, values) in profiles::BUILTIN {
        for (key, raw) in *values {
            let def = registry::find(key).unwrap_or_else(|| panic!("{name}: {key}"));
            assert_eq!(def.scope, Scope::Any, "{name}: {key}");
            assert!(*key != "profile", "{name}");
            value::check(def, raw).unwrap_or_else(|e| panic!("{name}: {key}: {e}"));
        }
    }
}

#[tokio::test]
async fn a_config_file_error_says_what_failed_and_names_the_file() {
    let fs = files(&[(NAMED, "model = ")]);
    let err = bare().file(NAMED).load(&fs).await.unwrap_err();
    assert_eq!(err.to_string(), format!("{NAMED} is not valid TOML"));
    assert!(
        std::error::Error::source(&err).is_some(),
        "the parser's words are the source"
    );
}
