//! The `haest` command's way past each limit: the setting that sets the cap,
//! or the slash command that gets past it. haest's own message says what
//! happened; this is added after it.

use haest::{
    config::DEFS,
    core::{ledger::ConversationCap, remedy::Limit},
};

/// What the human can do about `limit` here; None for a limit with no way past it.
pub fn remedy(limit: Limit) -> Option<String> {
    Some(match limit {
        Limit::TurnSteps(_) => sets_the_cap("max_turn_steps"),
        Limit::TurnTime(_) => sets_the_cap("max_turn_minutes"),
        Limit::TurnUsd(_) => sets_the_cap("max_turn_usd"),
        Limit::TurnTokens(_) => sets_the_cap("max_turn_tokens"),
        Limit::Repeats(_) => sets_the_cap("max_repeats"),
        Limit::RequestTokens(_) => format!(
            "/compact or /clear makes it smaller. {}",
            sets_the_cap("max_request_tokens")
        ),
        Limit::Conversation(ConversationCap::Usd(cap)) => {
            format!("Raise it with /budget {}.", cap * 2.0)
        }
        Limit::Conversation(ConversationCap::UnpricedTokens(cap)) => {
            format!("Raise it with /budget tokens {}.", cap * 2)
        }
        Limit::AgentTurns(_) => sets_the_cap("max_agent_turns"),
        Limit::SandboxEdits => format!(
            "The human reviews them with /changes and puts them on disk with /apply, and can relaunch haest with --{} real for the real file system.",
            setting("workspace").flag()
        ),
        _ => return None,
    })
}

fn setting(key: &str) -> &'static haest::config::Def {
    DEFS.iter()
        .find(|d| d.key == key)
        .unwrap_or_else(|| panic!("no setting {key}"))
}

/// The setting `key`, by its name in a config file and its variable.
fn sets_the_cap(key: &str) -> String {
    format!("The {key} setting ({}) sets the cap.", setting(key).env())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_limit_names_a_setting_haest_has() {
        let limits = [
            Limit::TurnSteps(1),
            Limit::TurnTime(std::time::Duration::from_secs(60)),
            Limit::TurnUsd(1.0),
            Limit::TurnTokens(1),
            Limit::Repeats(1),
            Limit::RequestTokens(1),
            Limit::Conversation(ConversationCap::Usd(1.0)),
            Limit::Conversation(ConversationCap::UnpricedTokens(1)),
            Limit::AgentTurns(1),
            Limit::SandboxEdits,
        ];
        for limit in limits {
            assert!(remedy(limit).is_some(), "{limit:?}");
        }
    }

    #[test]
    fn the_budget_remedy_raises_the_cap_in_force() {
        let text = remedy(Limit::Conversation(ConversationCap::Usd(5.0))).unwrap();
        assert!(text.contains("/budget 10"), "{text}");
    }
}
