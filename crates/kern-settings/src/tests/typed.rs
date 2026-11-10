//! The typed values: caps, windows, subagents, GenerateImage, web search.

use super::*;

async fn with_env(env: &[(&str, &str)]) -> Settings {
    run("/w", &[], env, &[]).await.unwrap().settings
}

#[tokio::test]
async fn token_caps_follow_the_window_unless_set() {
    let s = bare().await;
    let l = s.limits_for(200_000);
    assert_eq!(
        (l.max_request_tokens, l.max_turn_tokens),
        (180_000, 2_000_000)
    );
    let set = with_env(&[("KERN_MAX_REQUEST_TOKENS", "0")]).await;
    assert_eq!(set.limits_for(200_000).max_request_tokens, 0);
    assert_eq!(s.compact_at, 400_000);
    assert_eq!(s.data_dir, PathBuf::from("/h/.local/share/kern"));
}

#[tokio::test]
async fn agent_turns_and_subagent_caps_default_as_the_ts_kern_and_follow_their_variables() {
    let s = bare().await;
    assert_eq!(s.max_agent_turns, 30);
    let c = s.subagents;
    assert_eq!(
        (c.max_depth, c.max_concurrent, c.usd, c.steps, c.minutes),
        (2, 4, 1.0, 50, 15)
    );
    let s = with_env(&[
        ("KERN_MAX_AGENT_TURNS", "0"),
        ("KERN_MAX_AGENT_DEPTH", "0"),
        ("KERN_MAX_SUBAGENTS", "9"),
        ("KERN_SUBAGENT_USD", "0.25"),
        ("KERN_SUBAGENT_STEPS", "7"),
        ("KERN_SUBAGENT_MINUTES", "3"),
    ])
    .await;
    assert_eq!(s.max_agent_turns, 0);
    let c = s.subagents;
    assert_eq!(
        (c.max_depth, c.max_concurrent, c.usd, c.steps, c.minutes),
        (0, 9, 0.25, 7, 3)
    );
}

#[tokio::test]
async fn code_web_search_and_switch_words_read_as_asked() {
    let s = with_env(&[
        ("KERN_CODE", "on"),
        ("KERN_CODE_TOOLS", " Bash , mcp__gh__* ,"),
        ("KERN_WEB_SEARCH", "exa"),
        ("KERN_APPROVALS", "off"),
        ("KERN_PROJECT_MODS", "1"),
    ])
    .await;
    assert_eq!(s.code, Some(vec!["Bash".into(), "mcp__gh__*".into()]));
    let w = s.web_search.unwrap();
    assert_eq!(
        (w.engine, w.max_results, w.max_total_results),
        (kern_core::provider::WebSearchEngine::Exa, 5, 15)
    );
    assert!(!s.approvals && s.project_mods);
}

#[tokio::test]
async fn generate_image_needs_the_switch_and_a_key_and_says_when_the_key_is_missing() {
    assert_eq!(with_env(&[("OPENROUTER_API_KEY", "k")]).await.image, None);
    let no_key = with_env(&[("KERN_IMAGE", "1")]).await;
    assert_eq!(no_key.image, None);
    assert_eq!(no_key.notes.len(), 1);
    let img = with_env(&[("KERN_IMAGE", "1"), ("OPENROUTER_API_KEY", "k")])
        .await
        .image
        .unwrap();
    use kern_tools::image::{DEFAULT_IMAGE_BASE_URL, DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS};
    assert_eq!(
        (img.api_key.as_str(), img.base_url.as_str()),
        ("k", DEFAULT_IMAGE_BASE_URL)
    );
    assert_eq!(img.model, DEFAULT_IMAGE_MODEL);
    assert_eq!(img.models, DEFAULT_IMAGE_MODELS.map(String::from).to_vec());
    assert!(
        !format!("{img:?}").contains("\"k\""),
        "the key never prints"
    );
}

#[tokio::test]
async fn image_models_put_the_default_first_once() {
    let img = |extra: &'static [(&'static str, &'static str)]| async move {
        let mut env = vec![("KERN_IMAGE", "true"), ("OPENROUTER_API_KEY", "k")];
        env.extend_from_slice(extra);
        with_env(&env).await.image.unwrap()
    };
    let i = img(&[
        ("KERN_IMAGE_MODEL", "x/pic"),
        ("KERN_IMAGE_MODELS", " a/one , x/pic,, b/two "),
        ("OPENROUTER_BASE_URL", "http://local/v1"),
    ])
    .await;
    assert_eq!(i.models, ["x/pic", "a/one", "b/two"]);
    assert_eq!(i.base_url, "http://local/v1");
    let only = img(&[("KERN_IMAGE_MODEL", "x/pic"), ("KERN_IMAGE_MODELS", "none")]).await;
    assert_eq!(only.models, ["x/pic"]);
    let unlisted = img(&[("KERN_IMAGE_MODEL", "x/pic")]).await;
    assert_eq!(
        (unlisted.models.len(), unlisted.models[0].as_str()),
        (4, "x/pic")
    );
}

#[test]
fn every_setting_is_one_name_spelled_three_ways() {
    let d = registry::find("skill_catalog").unwrap();
    assert_eq!(
        (d.env().as_str(), d.flag().as_str()),
        ("KERN_SKILL_CATALOG", "skill-catalog")
    );
    for d in DEFS {
        assert!(
            d.key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{}",
            d.key
        );
        if let Some(raw) = d.default {
            assert!(value::check(d, raw).is_ok(), "{}'s default checks", d.key);
        }
    }
}
