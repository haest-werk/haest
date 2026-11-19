//! `wind_down_at`: a share from 0 to 1, and the setting every limit's message
//! names exists.

use std::time::Duration;

use haest_core::{caps::ConversationCap, remedy::Limit, wind_down::WindDown};

use super::*;

#[tokio::test]
async fn a_share_above_one_is_refused_naming_where_it_came_from() {
    let err = run("/w", &[], &[("HAEST_WIND_DOWN_AT", "1.5")], &[])
        .await
        .unwrap_err();
    let var = Source::Env("HAEST_WIND_DOWN_AT".into());
    assert!(
        matches!(&err, ConfigError::Invalid { at, key, .. } if *at == var && key == "wind_down_at"),
        "{err:?}"
    );
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
