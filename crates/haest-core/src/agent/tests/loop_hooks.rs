//! The hooks around a model call's answer: `after_model_call` before its
//! tools run, and `before_turn_end` when a turn would end on its own.

use std::sync::Mutex;

use super::*;
use crate::{messages::Usage, remedy::Limit};

/// An agent on `p` with the logging tool (a write: its calls run one by one)
/// and `caps`, asking `host`.
fn asked(p: &ScriptedProvider, host: &Arc<Asking>, caps: Option<TurnCaps>) -> Agent {
    let opts = Opts {
        tools: Some(vec![logging_tool(&host.seen, false)]),
        caps,
        ..Default::default()
    };
    let (mut agent, _) = agent_with(p, opts);
    agent.map_deps(|d| d.with_hooks(host.clone()));
    agent
}

fn seen(host: &Asking) -> Vec<Asked> {
    host.seen.lock().unwrap().clone()
}

fn answer(calls: &[&str]) -> Asked {
    Asked::Answer {
        calls: calls.iter().map(ToString::to_string).collect(),
        last: true,
    }
}

fn tool(id: &str) -> Asked {
    Asked::Tool(id.into())
}

fn log_calls(ids: &[&str]) -> crate::messages::AssistantMessage {
    let calls: Vec<_> = ids.iter().map(|id| (*id, "Log", json!({}))).collect();
    tool_answer(&calls)
}

#[tokio::test]
async fn the_hook_after_a_model_call_sees_the_answer_last_in_history_before_its_tools_run() {
    let p = ScriptedProvider::new([log_calls(&["a", "b"]), text_answer("done")]);
    let host = Arc::new(Asking::default());
    let mut agent = asked(&p, &host, None);

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.stop_reason, StopReason::EndTurn);
    assert_eq!(
        seen(&host),
        [
            answer(&["a", "b"]),
            tool("a"),
            tool("b"),
            answer(&[]),
            Asked::TurnEnd
        ]
    );
}

#[tokio::test]
async fn an_error_from_the_hook_after_a_model_call_ends_the_turn_and_its_calls_never_run() {
    let p = ScriptedProvider::new([log_calls(&["a"]), text_answer("never")]);
    let host = Arc::new(Asking {
        refuse: Some("the answer breaks a rule"),
        ..Default::default()
    });
    let mut agent = asked(&p, &host, None);

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.stop_reason, StopReason::Error);
    assert_eq!(ended.error.as_deref(), Some("the answer breaks a rule"));
    assert_eq!(seen(&host), [answer(&["a"])], "no tool ran, no end asked");
    assert_eq!(p.requests().len(), 1);
    // The call still has its result, so the next turn's request is accepted.
    assert_eq!(roles(agent.history()), ["user", "assistant", "tool"]);
    assert!(matches!(&agent.history()[2], Message::Tool(r) if r.is_error));
}

#[tokio::test]
async fn a_turn_aborted_while_the_hook_after_a_model_call_ran_answers_its_calls_unrun() {
    let p = ScriptedProvider::new([log_calls(&["a"]), text_answer("never")]);
    let host = Arc::new(Asking {
        abort: true,
        ..Default::default()
    });
    let mut agent = asked(&p, &host, None);

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.stop_reason, StopReason::Aborted);
    assert_eq!(seen(&host), [answer(&["a"])]);
    assert!(matches!(&agent.history()[2], Message::Tool(r) if r.is_error));
}

#[tokio::test]
async fn the_hook_before_a_turn_ends_may_keep_it_going_with_a_user_message() {
    let p = ScriptedProvider::new([text_answer("half way"), text_answer("all of it")]);
    let host = Arc::new(Asking {
        go_on: Mutex::new(vec!["check the rest"]),
        ..Default::default()
    });
    let mut agent = asked(&p, &host, None);
    let mut events = Vec::new();

    let ended = agent.run("go", &mut events).await;

    assert_eq!(ended.stop_reason, StopReason::EndTurn);
    // Asked twice: Some went on, None ended the turn.
    assert_eq!(
        seen(&host),
        [answer(&[]), Asked::TurnEnd, answer(&[]), Asked::TurnEnd]
    );
    let sent = &p.requests()[1].messages;
    assert_eq!(roles(sent), ["user", "assistant", "user"]);
    assert!(matches!(&sent[2], Message::User(u) if u.content == "check the rest"));
    assert_eq!(count(&events, |e| matches!(e, LoopEvent::User { .. })), 2);
}

#[tokio::test]
async fn the_hook_before_a_turn_ends_is_not_asked_when_it_is_aborted() {
    let p = ScriptedProvider::new([text_answer("a")]);
    let host = Arc::new(Asking {
        abort: true,
        always: Some("go on"),
        ..Default::default()
    });
    let mut agent = asked(&p, &host, None);

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.stop_reason, StopReason::Aborted);
    assert!(!seen(&host).contains(&Asked::TurnEnd));
}

#[tokio::test]
async fn neither_hook_is_asked_about_a_failed_answer() {
    let mut failed = text_answer("half");
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("the stream broke".into());
    let p = ScriptedProvider::new([failed]);
    let host = Arc::new(Asking {
        always: Some("go on"),
        ..Default::default()
    });
    let mut agent = asked(&p, &host, None);

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.stop_reason, StopReason::Error);
    assert_eq!(seen(&host), []);
}

#[tokio::test]
async fn the_hook_before_a_turn_ends_is_not_asked_when_a_cap_stops_it() {
    let p = ScriptedProvider::new([log_calls(&["a"]), text_answer("never")]);
    let host = Arc::new(Asking {
        always: Some("go on"),
        ..Default::default()
    });
    let mut agent = asked(&p, &host, Some(turn_caps(|c| c.max_turn_steps = 1)));

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.limit, Some(Limit::TurnSteps(1)));
    assert_eq!(seen(&host), [answer(&["a"]), tool("a")]);
}

#[tokio::test]
async fn a_hook_that_always_keeps_the_turn_going_is_stopped_by_the_turns_caps() {
    let p = ScriptedProvider::new([text_answer("1"), text_answer("2"), text_answer("3")]);
    let host = Arc::new(Asking {
        always: Some("again"),
        ..Default::default()
    });
    let mut agent = asked(&p, &host, Some(turn_caps(|c| c.max_turn_steps = 3)));

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.stop_reason, StopReason::MaxTokens);
    assert_eq!(ended.limit, Some(Limit::TurnSteps(3)));
    assert_eq!(p.requests().len(), 3);
    let asked = seen(&host).iter().filter(|s| **s == Asked::TurnEnd).count();
    assert_eq!(asked, 3);
}

#[tokio::test]
async fn what_the_hook_after_a_model_call_spends_stops_the_turn_before_its_next_call() {
    let p = ScriptedProvider::new([log_calls(&["a"]), text_answer("never")]);
    let host = Arc::new(Asking {
        spend: Some(Usage {
            input: 1_000,
            ..Usage::default()
        }),
        ..Default::default()
    });
    let mut agent = asked(&p, &host, Some(turn_caps(|c| c.max_turn_tokens = 500)));

    let ended = agent.run("go", &mut Vec::new()).await;

    assert_eq!(ended.limit, Some(Limit::TurnTokens(500)));
    assert_eq!(p.requests().len(), 1);
    // The answer's call still ran: a turn stops before a model call.
    assert_eq!(seen(&host), [answer(&["a"]), tool("a")]);
}
