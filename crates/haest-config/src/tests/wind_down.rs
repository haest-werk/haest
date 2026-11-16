//! `wind_down_at`: off unless asked for (the claude profile too), a share
//! from 0 to 1, and the setting every limit's message names exists.

use std::time::Duration;

use haest_core::{caps::ConversationCap, remedy::Limit, wind_down::WindDown};

use super::*;

#[tokio::test]
async fn wind_down_is_off_in_bare_haest_and_in_the_claude_profile() {
    assert_eq!(settings(&[], &[]).await.wind_down_at, WindDown::OFF);
    let claude = settings(&[], &[("profile", "claude")]).await;
    assert_eq!(claude.wind_down_at, WindDown::OFF);
}

#[tokio::test]
async fn a_share_from_a_file_a_variable_or_a_flag_turns_it_on() {
    let at = |s: Settings| s.wind_down_at.share();
    let file = [(USER_FILE, "wind_down_at = 0.8")];
    let from_file = run("/w", &file, &[], &[]).await.unwrap().config.settings;
    assert_eq!(at(from_file), 0.8);
    assert_eq!(
        at(settings(&[("HAEST_WIND_DOWN_AT", "0.5")], &[]).await),
        0.5
    );
    assert_eq!(at(settings(&[], &[("wind_down_at", "1")]).await), 1.0);
}

#[tokio::test]
async fn a_share_above_one_is_refused_naming_where_it_came_from() {
    let err = run("/w", &[], &[("HAEST_WIND_DOWN_AT", "1.5")], &[])
        .await
        .unwrap_err();
    let text = report(&err);
    assert!(text.contains("HAEST_WIND_DOWN_AT"), "{text}");
    let mut c = Config::bare("/w", HOME);
    assert!(c.set("wind_down_at", "1.5").is_err());
    assert_eq!(c.settings.wind_down_at, WindDown::OFF, "nothing changed");
}

/// haest's messages name the setting of each limit (`Limit::setting`): it
/// must be one haest has.
#[test]
fn every_limit_names_a_setting_in_the_registry() {
    let limits = [
        Limit::TurnSteps(1),
        Limit::TurnTime(Duration::from_secs(60)),
        Limit::TurnUsd(1.0),
        Limit::TurnTokens(1),
        Limit::Repeats(1),
        Limit::RequestTokens(1),
        Limit::Conversation(ConversationCap::Usd(1.0)),
        Limit::Conversation(ConversationCap::UnpricedTokens(1)),
        Limit::AgentTurns(1),
        Limit::SandboxEdits,
        Limit::SubagentSteps(1),
        Limit::SubagentTime(Duration::from_secs(60)),
        Limit::SubagentUsd(1.0),
    ];
    for limit in limits {
        let key = limit.setting();
        assert!(DEFS.iter().any(|d| d.key == key), "{limit:?}: {key}");
    }
}
