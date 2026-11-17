//! Every setting reads into its typed field and reads back out as the same
//! text: what a file says is what `haest settings` shows.

use haest_core::provider::WebSearchEngine;

use super::*;
use crate::value::{check, var_parts};

#[tokio::test]
async fn caps_and_subagents_follow_their_variables() {
    let s = settings(
        &[
            ("HAEST_MAX_REQUEST_TOKENS", "0"),
            ("HAEST_MAX_TURN_MINUTES", "1.5"),
            ("HAEST_MAX_AGENT_TURNS", "0"),
            ("HAEST_MAX_AGENT_DEPTH", "0"),
            ("HAEST_MAX_SUBAGENTS", "9"),
            ("HAEST_SUBAGENT_USD", "0.25"),
            ("HAEST_SUBAGENT_STEPS", "7"),
            ("HAEST_SUBAGENT_MINUTES", "3"),
        ],
        &[],
    )
    .await;
    assert_eq!(s.turn_caps_for(200_000).max_request_tokens, 0);
    assert_eq!(
        s.turn_caps_for(200_000).max_turn_time,
        std::time::Duration::from_secs(90)
    );
    assert_eq!(s.max_agent_turns, 0);
    let c = s.subagents;
    assert_eq!(
        (c.max_depth, c.max_concurrent, c.usd, c.steps, c.time),
        (0, 9, 0.25, 7, std::time::Duration::from_secs(180))
    );
}

/// Both wall-clock caps read and write minutes, a fraction too; one too long
/// for a `Duration` is the longest there is, and a negative one is refused.
#[tokio::test]
async fn the_wall_clock_caps_are_minutes_either_way() {
    let s = settings(
        &[
            ("HAEST_MAX_TURN_MINUTES", "0.5"),
            ("HAEST_SUBAGENT_MINUTES", "2.5"),
        ],
        &[],
    )
    .await;
    assert_eq!(s.caps.max_turn_time, std::time::Duration::from_secs(30));
    assert_eq!(s.subagents.time, std::time::Duration::from_secs(150));
    let mut c = crate::Config::from(s);
    assert_eq!(c.get("max_turn_minutes").as_deref(), Some("0.5"));
    assert_eq!(c.get("subagent_minutes").as_deref(), Some("2.5"));
    c.set("max_turn_minutes", "1e300").unwrap();
    assert_eq!(c.settings.caps.max_turn_time, std::time::Duration::MAX);
    assert!(c.set("subagent_minutes", "-1").is_err());
}

#[tokio::test]
async fn code_web_search_and_switch_words_read_as_asked() {
    let s = settings(
        &[
            ("HAEST_CODE", "on"),
            ("HAEST_CODE_TOOLS", " Bash , mcp__gh__* ,"),
            ("HAEST_WEB_SEARCH", "exa"),
            ("HAEST_APPROVALS", "off"),
            ("HAEST_IMAGE_MODELS", "none"),
        ],
        &[],
    )
    .await;
    assert!(s.code);
    assert_eq!(s.code_tools, ["Bash", "mcp__gh__*"]);
    assert_eq!(s.web_search, Some(WebSearchEngine::Exa));
    assert!(!s.approvals);
    assert_eq!(s.image_models, Some(Vec::new()));
}

/// Every setting's bare value round-trips, and its example (what the settings
/// reference shows) is a value it takes that is not the bare one.
#[test]
fn every_setting_reads_back_as_the_text_it_was_set_from() {
    let bare = Config::bare("/w", "/h");
    for def in DEFS {
        if let Some(raw) = bare.get(def.key) {
            let mut c = bare.clone();
            c.set(def.key, &raw)
                .unwrap_or_else(|e| panic!("{}: {e}", def.key));
            assert_eq!(c, bare, "{} = {raw}", def.key);
        }
        let raw = check(def, def.example).unwrap_or_else(|e| panic!("{}: {e}", def.key));
        let mut c = bare.clone();
        c.set(def.key, &raw).unwrap();
        assert_ne!(c, bare, "{}: the example {raw} is the bare value", def.key);
        let shown = match def.kind {
            // A variable reads back as its name: its value may be a key.
            Kind::Vars => var_parts(&raw).0,
            _ => raw.as_str(),
        };
        assert_eq!(c.get(def.key).as_deref(), Some(shown), "{}", def.key);
    }
}

#[test]
fn every_word_a_choice_offers_is_one_its_field_takes() {
    for def in DEFS {
        let Kind::Choice(words) = def.kind else {
            continue;
        };
        for word in words {
            let mut c = Config::bare("/w", "/h");
            c.set(def.key, word)
                .unwrap_or_else(|e| panic!("{} = {word}: {e}", def.key));
            assert_eq!(c.get(def.key).as_deref(), Some(*word), "{}", def.key);
        }
    }
}

#[test]
fn an_unknown_name_is_refused() {
    let mut c = Config::bare("/w", "/h");
    assert!(c.set("tool", "read").is_err());
    assert_eq!(c.get("tool"), None);
}

#[test]
fn every_setting_is_one_name_spelled_three_ways() {
    let d = registry::find("skill_catalog").unwrap();
    assert_eq!(
        (d.env().as_str(), d.flag().as_str()),
        ("HAEST_SKILL_CATALOG", "skill-catalog")
    );
    for d in DEFS {
        assert!(
            d.key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{}",
            d.key
        );
    }
}
