//! One tool-result flood cost ~$50 in a single turn: nothing stopped a turn from
//! spending without bound. These pin that something does.

use serde_json::json;

use super::*;
use crate::{
    agent::test_support::*,
    events::LoopEvent,
    messages::{
        AssistantMessage, AssistantPart, MediaKind, MediaPart, StopReason, Usage, UserMessage,
    },
    remedy::Limit,
    testing::{ScriptedProvider, text_answer},
    tool::ToolOutput,
};

/// A model that calls a tool forever, billing `usage` per call.
fn looping(usage: Usage) -> ScriptedProvider {
    ScriptedProvider::new(
        (0..50).map(|i| with_usage(echo(&format!("t{i}"), json!({"v": "x"})), &usage)),
    )
}

fn usage(input: u64, output: u64, cost: Option<f64>) -> Usage {
    Usage {
        input,
        output,
        cost_usd: cost,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_turn_stops_once_its_reported_cost_reaches_the_usd_cap() {
    let p = looping(usage(1, 1, Some(1.0)));
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_usd = 2.5));
    let (stop, _, error) = end(&collect(&mut agent, "go").await);
    assert_eq!(p.requests().len(), 3);
    assert_eq!(stop, StopReason::MaxTokens);
    assert!(error.is_some_and(|e| !e.is_empty()));
}

/// A provider's bill that is no real amount switched the turn's USD cap off
/// (a NaN: `NaN >= cap` is false for the rest of the turn), lowered what the
/// turn had spent (a refund), or stopped it at once (an infinity).
#[tokio::test]
async fn a_cost_that_is_no_real_amount_is_no_price_to_the_turns_usd_cap() {
    for bad in [f64::NAN, -50.0, f64::INFINITY] {
        let billed = |i: usize, cost| {
            with_usage(
                echo(&format!("t{i}"), json!({"v": "x"})),
                &usage(1, 1, Some(cost)),
            )
        };
        let p = ScriptedProvider::new(
            std::iter::once(billed(0, bad)).chain((1..50).map(|i| billed(i, 1.0))),
        );
        let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_usd = 2.5));
        end(&collect(&mut agent, "go").await);
        // $0, $1, $2, then $3 is past the cap: no fifth call.
        assert_eq!(p.requests().len(), 4, "a first bill of {bad}");
    }
}

#[tokio::test]
async fn the_token_cap_stops_a_turn_on_a_provider_that_reports_no_cost() {
    let p = looping(usage(10, 5, None));
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_tokens = 30));
    end(&collect(&mut agent, "go").await);
    assert_eq!(p.requests().len(), 2);
}

#[tokio::test]
async fn a_stopped_turn_leaves_every_tool_call_answered_so_the_next_turn_can_be_sent() {
    let p = looping(usage(10, 5, None));
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_tokens = 30));
    collect(&mut agent, "go").await;
    let calls: usize = agent
        .history()
        .iter()
        .map(|m| match m {
            Message::Assistant(a) => a.tool_calls().count(),
            _ => 0,
        })
        .sum();
    let results = agent
        .history()
        .iter()
        .filter(|m| matches!(m, Message::Tool(_)))
        .count();
    assert_eq!(results, calls);
}

#[tokio::test]
async fn a_tools_own_spend_counts_toward_the_turn_and_its_usd_cap() {
    let paid = FnTool::arc("Echo", false, |_, _| async {
        Ok(ToolOutput {
            cost_usd: Some(1.0),
            ..ToolOutput::text("drew it")
        })
    });
    let p = looping(usage(1, 1, None));
    let mut opts = capped(|l| l.max_turn_usd = 2.5);
    opts.tools = Some(vec![paid]);
    let (mut agent, _) = agent_with(&p, opts);
    let (_, usage, _) = end(&collect(&mut agent, "go").await);
    assert_eq!(p.requests().len(), 3);
    assert_eq!(usage.cost_usd, Some(3.0));
}

#[tokio::test]
async fn the_cap_is_per_turn_the_next_message_starts_from_zero() {
    let p = looping(usage(10, 5, None));
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_tokens = 30));
    collect(&mut agent, "go").await;
    collect(&mut agent, "continue").await;
    assert_eq!(p.requests().len(), 4);
}

#[tokio::test]
async fn an_oversized_request_is_never_sent_and_nothing_is_added_to_history() {
    let p = ScriptedProvider::new([text_answer("never")]);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_request_tokens = 100));
    let (stop, _, _) = end(&collect(&mut agent, &"x".repeat(2000)).await);
    assert!(p.requests().is_empty());
    assert_eq!(stop, StopReason::Error);
    assert_eq!(agent.history().len(), 1);
    assert!(matches!(&agent.history()[0], Message::User(_)));
}

/// A request too big to send never went out, so no `ModelStart` says it
/// did: the same as a spec with no provider. One used to come first.
#[tokio::test]
async fn an_oversized_request_announces_no_model_call() {
    let p = ScriptedProvider::new([text_answer("never")]);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_request_tokens = 100));
    let events = collect(&mut agent, &"x".repeat(2000)).await;
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            LoopEvent::User { .. } => "user",
            LoopEvent::ModelStart { .. } => "model_start",
            LoopEvent::Assistant { .. } => "assistant",
            LoopEvent::TurnEnd { .. } => "turn_end",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["user", "assistant", "turn_end"]);
}

/// The turn's ending names the cap, so a host need not read the text.
#[tokio::test]
async fn a_turn_stopped_by_a_cap_names_the_limit() {
    let p = ScriptedProvider::new([text_answer("never")]);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_request_tokens = 100));
    let mut events = Vec::new();
    let too_big = agent.run(&"x".repeat(2000), &mut events).await;
    assert_eq!(too_big.limit, Some(Limit::RequestTokens(100)));

    let paid = usage(1, 1, Some(1.0));
    let p = looping(paid);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_usd = 1.5));
    let over = agent.run("go", &mut events).await;
    assert_eq!(over.stop_reason, StopReason::MaxTokens);
    assert_eq!(over.limit, Some(Limit::TurnUsd(1.5)));

    let p = ScriptedProvider::new([text_answer("fine")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    assert_eq!(agent.run("hi", &mut events).await.limit, None);
}

#[tokio::test]
async fn a_request_under_the_cap_goes_out() {
    let p = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_request_tokens = 10_000));
    collect(&mut agent, "hi").await;
    assert_eq!(p.requests().len(), 1);
}

#[test]
fn an_image_counts_as_its_token_cost_not_its_base64() {
    let big = MediaPart {
        kind: MediaKind::Image,
        media_type: "image/png".into(),
        data: "A".repeat(1_000_000),
        name: "shot.png".into(),
    };
    let with_image = [Message::User(UserMessage {
        content: "look".into(),
        media: vec![big],
    })];
    let estimate = estimate_request_tokens("", &with_image, &[]);
    assert!(estimate > 1600 && estimate < 2000, "{estimate}");
    let text_only = [Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::Text {
            text: "x".repeat(4000),
        }],
        ..AssistantMessage::start("p", "m")
    })];
    assert!(estimate_request_tokens("", &text_only, &[]) >= 1000);
}

#[test]
fn a_zero_request_cap_refuses_nothing() {
    assert_eq!(request_too_big(u64::MAX, &uncapped()), None);
    let l = turn_caps(|l| l.max_request_tokens = 10);
    assert_eq!(request_too_big(10, &l), None);
    assert!(request_too_big(11, &l).is_some());
}
