//! Every setting reads into its typed field and reads back out as the same
//! text: what a file says is what `kern settings` shows.

use kern_core::provider::WebSearchEngine;

use super::*;
use crate::value::check;

#[tokio::test]
async fn caps_and_subagents_follow_their_variables() {
    let s = settings(
        &[
            ("KERN_MAX_REQUEST_TOKENS", "0"),
            ("KERN_MAX_TURN_MINUTES", "1.5"),
            ("KERN_MAX_AGENT_TURNS", "0"),
            ("KERN_MAX_AGENT_DEPTH", "0"),
            ("KERN_MAX_SUBAGENTS", "9"),
            ("KERN_SUBAGENT_USD", "0.25"),
            ("KERN_SUBAGENT_STEPS", "7"),
            ("KERN_SUBAGENT_MINUTES", "3"),
        ],
        &[],
    )
    .await;
    assert_eq!(s.limits_for(200_000).max_request_tokens, 0);
    assert_eq!(s.limits_for(200_000).max_turn_ms, 90_000);
    assert_eq!(s.max_agent_turns, 0);
    let c = s.subagents;
    assert_eq!(
        (c.max_depth, c.max_concurrent, c.usd, c.steps, c.minutes),
        (0, 9, 0.25, 7, 3)
    );
}

#[tokio::test]
async fn code_web_search_and_switch_words_read_as_asked() {
    let s = settings(
        &[
            ("KERN_CODE", "on"),
            ("KERN_CODE_TOOLS", " Bash , mcp__gh__* ,"),
            ("KERN_WEB_SEARCH", "exa"),
            ("KERN_APPROVALS", "off"),
            ("KERN_IMAGE_MODELS", "none"),
        ],
        &[],
    )
    .await;
    assert_eq!(
        s.code_tools(),
        Some(vec!["Bash".into(), "mcp__gh__*".into()])
    );
    assert_eq!(s.web_search, Some(WebSearchEngine::Exa));
    assert!(!s.approvals);
    assert_eq!(s.image_models, Some(Vec::new()));
}

/// A value of each kind that is not the bare one, as `check` spells it.
const SAMPLES: &[(&str, &str)] = &[
    ("profile", "claude"),
    ("model", "a:b"),
    ("effort", "xhigh"),
    ("system_prompt", "You are terse."),
    ("append_system_prompt", "Answer in French."),
    ("context", "agents"),
    ("skills", "claude,plugins"),
    ("tools", "read,bash,skill"),
    ("code_tools", "Bash,mcp__gh__*"),
    ("web_search", "perplexity"),
    ("image_models", "a/one,b/two"),
    ("deferred_tools", "grow"),
    ("workspace", "real"),
    ("env_passthrough", "none"),
    ("trusted_dirs", "/p,/q"),
    ("max_turn_usd", "0.5"),
    ("max_turn_tokens", "0"),
    ("max_request_tokens", "5000"),
    ("max_conversation_tokens", "123"),
    ("context_window", "1000000"),
    ("max_subagents", "9"),
    ("subagent_usd", "0.25"),
    ("data_dir", "/d"),
];

#[test]
fn every_setting_reads_back_as_the_text_it_was_set_from() {
    let bare = Config::bare("/w", "/h");
    for def in DEFS {
        // The bare value round-trips and leaves everything as it was.
        if let Some(raw) = get(&bare, def.key) {
            let mut c = bare.clone();
            set(&mut c, def.key, &raw).unwrap_or_else(|e| panic!("{}: {e}", def.key));
            assert_eq!(c, bare, "{} = {raw}", def.key);
        }
        let flipped = match def.kind {
            Kind::Bool => get(&bare, def.key).map(|v| (v == "false").to_string()),
            _ => None,
        };
        let sample = SAMPLES
            .iter()
            .find(|(k, _)| *k == def.key)
            .map(|(_, v)| v.to_string());
        assert!(
            get(&bare, def.key).is_some() || sample.is_some(),
            "{}: no bare value and no sample, so nothing here exercises it",
            def.key
        );
        for raw in flipped.into_iter().chain(sample) {
            let raw = check(def, &raw).unwrap();
            let mut c = bare.clone();
            set(&mut c, def.key, &raw).unwrap();
            assert_ne!(c, bare, "{} = {raw} changes nothing", def.key);
            assert_eq!(
                get(&c, def.key).as_deref(),
                Some(raw.as_str()),
                "{}",
                def.key
            );
        }
    }
}

#[test]
fn an_unknown_name_is_refused() {
    let mut c = Config::bare("/w", "/h");
    assert!(set(&mut c, "tool", "read").is_err());
    assert_eq!(get(&c, "tool"), None);
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
    }
}
