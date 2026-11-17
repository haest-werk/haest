//! Bare is every capability off and every brake on; the caps follow the window;
//! a word set answers by word in one order.

use std::path::PathBuf;

use haest_core::{WorkspaceMode, provider::WebSearchEngine};

use super::*;

fn bare() -> Settings {
    Settings::bare("/w", "/h")
}

#[test]
fn bare_turns_every_capability_off_and_every_brake_on() {
    let s = bare();
    assert!(s.tools.is_empty() && s.context.is_empty() && s.skills.is_empty());
    assert!(!s.skill_catalog && !s.agents && !s.code && !s.image && !s.mods && !s.project_mods);
    assert_eq!(s.web_search_options(), None);
    assert!(s.approvals);
    assert_eq!(s.workspace, WorkspaceMode::Sandbox);
    assert_eq!(s.caps.max_turn_usd, 2.0);
    assert_eq!(s.model_spec, DEFAULT_MODEL);
    assert_eq!(
        (s.data_dir, s.config_dir),
        (
            PathBuf::from("/h/.local/share/haest"),
            PathBuf::from("/h/.config/haest")
        )
    );
}

/// The caps' default is bare's, so `..CapSettings::default()` keeps every
/// brake bare has.
#[test]
fn the_default_caps_are_the_bare_caps() {
    let caps = CapSettings::default();
    assert_eq!(caps, bare().caps);
    assert_eq!(
        (caps.max_turn_usd, caps.max_turn_steps, caps.max_turn_time),
        (2.0, 100, std::time::Duration::from_secs(60 * 60))
    );
    assert_eq!(
        (
            caps.max_repeats,
            caps.max_conversation_usd,
            caps.max_conversation_tokens
        ),
        (6, 20.0, 20_000_000)
    );
    assert_eq!(
        (
            caps.max_turn_tokens,
            caps.max_request_tokens,
            caps.context_window
        ),
        (None, None, None)
    );
    assert_eq!(caps.compact_at, 400_000);
}

#[test]
fn token_caps_follow_the_window_unless_set() {
    let mut s = bare();
    let l = s.turn_caps_for(200_000);
    assert_eq!(
        (l.max_request_tokens, l.max_turn_tokens),
        (180_000, 2_000_000)
    );
    assert_eq!(l.max_turn_time, std::time::Duration::from_secs(3600));
    s.caps.max_request_tokens = Some(0);
    assert_eq!(s.turn_caps_for(200_000).max_request_tokens, 0);
    assert_eq!(
        (s.window(None), s.window(Some(1_000_000))),
        (200_000, 1_000_000)
    );
    s.caps.context_window = Some(50_000);
    assert_eq!(s.window(Some(1_000_000)), 50_000);
}

/// `context_window` takes any u64: a huge one saturates the caps that follow
/// it, and a window that is not a multiple of 10 keeps its exact 90%.
#[test]
fn token_caps_on_a_huge_window_saturate_instead_of_overflowing() {
    let s = bare();
    let l = s.turn_caps_for(u64::MAX);
    assert_eq!(l.max_turn_tokens, u64::MAX);
    assert_eq!(l.max_request_tokens, (u128::from(u64::MAX) * 9 / 10) as u64);
    let l = s.turn_caps_for(2_000_000_000_000_000_000);
    assert_eq!(
        (l.max_request_tokens, l.max_turn_tokens),
        (1_800_000_000_000_000_000, u64::MAX)
    );
    assert_eq!(s.turn_caps_for(1_048_576).max_request_tokens, 943_718);
}

#[test]
fn agent_turns_and_subagent_caps_default_as_the_ts_haest() {
    let s = bare();
    assert_eq!(s.max_agent_turns, 30);
    let c = s.subagents;
    assert_eq!(
        (c.max_depth, c.max_concurrent, c.usd, c.steps, c.time),
        (2, 4, 1.0, 50, std::time::Duration::from_secs(15 * 60))
    );
    assert_eq!(s.subagent_caps().time, c.time);
    assert!(
        !c.budgets && !s.subagent_caps().budgets,
        "budgets are opt-in"
    );
}

#[test]
fn web_search_is_off_until_switched_on() {
    let s = Settings {
        web_search: Some(WebSearchEngine::Exa),
        ..bare()
    };
    let w = s.web_search_options().unwrap();
    assert_eq!(
        (w.engine, w.max_results, w.max_total_results),
        (WebSearchEngine::Exa, 5, 15)
    );
}

#[test]
fn a_word_set_answers_by_word_in_one_order_and_refuses_a_word_it_does_not_know() {
    let t = BuiltinTools::from_words(["bash", "read"]).unwrap();
    assert!(t.read && t.bash && !t.write);
    assert_eq!(t.words(), ["read", "bash"]);
    assert_eq!(BuiltinTools::ALL.words(), BuiltinTools::WORDS);
    assert_eq!(BuiltinTools::from_words(["bsh"]).unwrap_err().word, "bsh");
    let s = SkillSources::from_words(["plugins"]).unwrap();
    assert!(s.has("plugins") && !s.has("agents"));
    assert_eq!(SkillSources::ALL.words(), SkillSources::WORDS);
    assert_eq!(ContextSources::ALL.words(), ["agents", "claude"]);
}
