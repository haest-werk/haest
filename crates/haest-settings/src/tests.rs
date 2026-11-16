//! Bare is every capability off and every brake on; the caps follow the window;
//! a word set answers by word in one order.

use std::path::PathBuf;

use haest_core::{
    WorkspaceMode,
    media::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS},
    provider::WebSearchEngine,
};

use super::*;

fn bare() -> Settings {
    Settings::bare("/w", "/h")
}

#[test]
fn bare_turns_every_capability_off_and_every_brake_on() {
    let s = bare();
    assert!(s.tools.is_empty() && s.context.is_empty() && s.skills.is_empty());
    assert!(!s.skill_catalog && !s.agents && !s.code && !s.image && !s.mods && !s.project_mods);
    assert_eq!((s.web_search_config(), s.code_tools()), (None, None));
    assert!(s.approvals);
    assert_eq!(s.workspace, WorkspaceMode::Sandbox);
    assert_eq!(s.limits.max_turn_usd, 2.0);
    assert_eq!(s.model, DEFAULT_MODEL);
    assert_eq!(
        (s.data_dir, s.config_dir),
        (
            PathBuf::from("/h/.local/share/haest"),
            PathBuf::from("/h/.config/haest")
        )
    );
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
    s.limits.max_request_tokens = Some(0);
    assert_eq!(s.turn_caps_for(200_000).max_request_tokens, 0);
    assert_eq!(
        (s.window(None), s.window(Some(1_000_000))),
        (200_000, 1_000_000)
    );
    s.limits.context_window = Some(50_000);
    assert_eq!(s.window(Some(1_000_000)), 50_000);
}

#[test]
fn agent_turns_and_subagent_caps_default_as_the_ts_haest() {
    let s = bare();
    assert_eq!(s.max_agent_turns, 30);
    let c = s.subagents;
    assert_eq!(
        (c.max_depth, c.max_concurrent, c.usd, c.steps, c.minutes),
        (2, 4, 1.0, 50, 15)
    );
}

#[test]
fn code_and_web_search_are_off_until_switched_on() {
    let s = Settings {
        code: true,
        code_tools: vec!["mcp__gh__*".into()],
        web_search: Some(WebSearchEngine::Exa),
        ..bare()
    };
    assert_eq!(s.code_tools(), Some(vec!["mcp__gh__*".to_string()]));
    let w = s.web_search_config().unwrap();
    assert_eq!(
        (w.engine, w.max_results, w.max_total_results),
        (WebSearchEngine::Exa, 5, 15)
    );
}

#[test]
fn image_models_put_the_chosen_one_first_once() {
    assert_eq!(
        bare().image_models(),
        DEFAULT_IMAGE_MODELS.map(String::from).to_vec()
    );
    assert_eq!(bare().image_models()[0], DEFAULT_IMAGE_MODEL);
    let s = Settings {
        image_model: "x/pic".into(),
        image_models: Some(vec!["a/one".into(), "x/pic".into(), "b/two".into()]),
        ..bare()
    };
    assert_eq!(s.image_models(), ["x/pic", "a/one", "b/two"]);
    let only = Settings {
        image_models: Some(Vec::new()),
        ..s
    };
    assert_eq!(only.image_models(), ["x/pic"]);
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
