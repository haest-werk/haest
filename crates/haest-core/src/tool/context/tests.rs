//! What a tool learns of its turn's time: what is left of the wall-clock cap
//! on the host's clock, or nothing when the turn has none.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::json;

use crate::{
    agent::test_support::*,
    guard::TurnGuard,
    remedy::Remedies,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::ToolOutput,
};

#[test]
fn the_time_left_is_the_cap_less_what_the_hosts_clock_says_has_run() {
    let clock = Arc::new(TestHooks::default());
    clock.set_now(5_000);
    let guard = TurnGuard::new(clock.clone(), None, Remedies::default());
    let caps = Some(turn_caps(|l| l.max_turn_time = Duration::from_secs(60)));
    assert_eq!(
        guard.time_left(caps.as_ref()),
        Some(Duration::from_secs(60))
    );
    clock.set_now(15_000);
    assert_eq!(
        guard.time_left(caps.as_ref()),
        Some(Duration::from_secs(50))
    );
    clock.set_now(99_000);
    assert_eq!(guard.time_left(caps.as_ref()), Some(Duration::ZERO));
    assert_eq!(guard.time_left(Some(&uncapped())), None, "no time cap");
    assert_eq!(guard.time_left(None), None);
}

/// The time left each call of Probe saw.
async fn seen_by_a_tool(caps: Option<crate::caps::TurnCaps>) -> Vec<Option<Duration>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    let probe = FnTool::arc("Probe", false, move |_, ctx| {
        let kept = kept.clone();
        async move {
            kept.lock().unwrap().push(ctx.turn_time_left);
            Ok(ToolOutput::text("ok"))
        }
    });
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Probe", json!({}))]),
        text_answer("done"),
    ]);
    let opts = Opts {
        tools: Some(vec![probe]),
        caps,
        ..Default::default()
    };
    let (mut agent, _) = agent_with(&p, opts);
    collect(&mut agent, "go").await;
    seen.lock().unwrap().clone()
}

#[tokio::test]
async fn a_tool_in_the_loop_sees_its_turns_time_left_and_none_without_a_cap() {
    let capped = Some(turn_caps(|l| l.max_turn_time = Duration::from_secs(60)));
    // The test clock stands still: all of the cap is left.
    assert_eq!(
        seen_by_a_tool(capped).await,
        [Some(Duration::from_secs(60))]
    );
    assert_eq!(seen_by_a_tool(Some(uncapped())).await, [None]);
}
