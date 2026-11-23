//! What haest does to a cache plan before it is sent ([`CachePlan::checked`]),
//! whom it tells, and which requests the policy plans.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{
    Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools,
    agent::test_support::{Opts, TestHooks, agent_with, collect},
    catalog::{CacheFacts, ModelFacts},
    compaction::{CompactionOptions, compact},
    messages::{AssistantMessage, MediaKind, MediaPart, Message, StopReason, UserMessage},
    provider::ToolSpec,
    testing::{ScriptedProvider, text_answer},
    tool::ToolServices,
};
use CacheTtl::{FiveMinutes, OneHour};

fn tool() -> ToolSpec {
    ToolSpec {
        name: "read".into(),
        description: "Reads a file.".into(),
        parameters: json!({"type": "object"}),
    }
}

fn talk(n: usize) -> Vec<Message> {
    (0..n)
        .map(|i| Message::user(format!("message {i}")))
        .collect()
}

/// A model that caches from `minimum` tokens, for an hour too.
fn caches_from(minimum: u64) -> CacheFacts {
    CacheFacts::new()
        .with_min_cacheable_tokens(minimum)
        .with_ttl_1h(true)
}

#[test]
fn the_default_plan_goes_out_as_planned_on_a_request_long_enough() {
    let (tools, messages, system) = ([tool()], talk(3), "x".repeat(2_000));
    let facts = caches_from(512);
    let shape = RequestShape::new("m", &tools, &system, &messages).with_facts(&facts);
    let (plan, fixes) = CachePlan::default().checked(&shape);
    assert_eq!(plan, CachePlan::standard(FiveMinutes));
    assert!(fixes.is_empty(), "{fixes:?}");
}

#[test]
fn a_place_the_request_lacks_goes_unmarked_without_a_note() {
    let shape = RequestShape::new("m", &[], "sys", &[]);
    let (plan, fixes) = CachePlan::default().checked(&shape);
    assert_eq!(plan, CachePlan::none().with_system(FiveMinutes));
    assert!(fixes.is_empty(), "{fixes:?}");
}

#[test]
fn chosen_messages_are_kept_once_and_one_on_the_newest_joins_the_history() {
    let messages = talk(4);
    let asked = CachePlan::none()
        .with_message(1, FiveMinutes)
        .with_message(1, OneHour)
        .with_message(3, OneHour)
        .with_message(9, FiveMinutes)
        .with_history(FiveMinutes);
    let (plan, fixes) = asked.checked(&RequestShape::new("m", &[], "sys", &messages));
    assert_eq!(plan.messages, [(1, OneHour)], "the longer of the two");
    assert_eq!(plan.history, Some(OneHour), "the longer of history and 3");
    assert_eq!(
        fixes,
        [PlanFix::NoSuchMessage {
            index: 9,
            messages: 4
        }]
    );
    assert!(!fixes[0].is_warning());
}

#[test]
fn a_model_without_an_hour_gets_five_minutes_and_the_human_hears_it() {
    let messages = talk(3);
    let five_only = CacheFacts::new();
    let shape = RequestShape::new("m", &[], "sys", &messages).with_facts(&five_only);
    let asked = (CachePlan::none().with_system(OneHour))
        .with_message(1, OneHour)
        .with_history(FiveMinutes);
    let (plan, fixes) = asked.clone().checked(&shape);
    assert_eq!(
        (plan.system, plan.messages, plan.history),
        (Some(FiveMinutes), vec![(1, FiveMinutes)], Some(FiveMinutes))
    );
    let at = vec![Breakpoint::System, Breakpoint::Message(1)];
    assert_eq!(fixes, [PlanFix::NoHour { at }]);
    assert!(fixes[0].is_warning());
    // Facts unknown: the plan goes out as asked.
    let (plan, fixes) = asked
        .clone()
        .checked(&RequestShape::new("m", &[], "sys", &messages));
    assert_eq!((plan, fixes), (asked, vec![]));
}

/// The breakpoints of `fixes` that were dropped as too short.
fn dropped_short(fixes: &[PlanFix]) -> Vec<Breakpoint> {
    (fixes.iter())
        .map(|f| match f {
            PlanFix::BelowMinimum { at, .. } => *at,
            other => panic!("only BelowMinimum: {other:?}"),
        })
        .collect()
}

#[test]
fn a_breakpoint_the_request_is_surely_too_short_for_is_dropped() {
    let messages = talk(2);
    let facts = caches_from(4_096);
    let short = RequestShape::new("m", &[], "Be brief.", &messages).with_facts(&facts);
    let (plan, fixes) = CachePlan::default().checked(&short);
    assert_eq!(plan.breakpoints(), 0);
    assert_eq!(
        dropped_short(&fixes),
        [Breakpoint::System, Breakpoint::History]
    );
    assert!(fixes.iter().all(|f| !f.is_warning()));
    // A system prompt of 4,096 bytes may be 4,096 tokens: it and all after it stay.
    let system = "x".repeat(4_096);
    let long = RequestShape::new("m", &[], &system, &messages).with_facts(&facts);
    let (plan, fixes) = CachePlan::default().checked(&long);
    assert_eq!(plan, CachePlan::standard(FiveMinutes).without_tools());
    assert!(fixes.is_empty(), "{fixes:?}");
}

#[test]
fn tools_count_with_the_instructions_a_provider_adds_for_them() {
    let (tools, messages) = ([tool()], talk(1));
    let at_allowance = caches_from(TOOL_PROMPT_ALLOWANCE);
    let shape = RequestShape::new("m", &tools, "", &messages).with_facts(&at_allowance);
    let (plan, fixes) = CachePlan::default().checked(&shape);
    assert_eq!(plan, CachePlan::default(), "one tool may already be enough");
    assert!(fixes.is_empty(), "{fixes:?}");
    let higher = caches_from(TOOL_PROMPT_ALLOWANCE * 4);
    let (plan, fixes) = CachePlan::default().checked(&shape.with_facts(&higher));
    assert_eq!(plan.breakpoints(), 0);
    let all = [Breakpoint::Tools, Breakpoint::System, Breakpoint::History];
    assert_eq!(dropped_short(&fixes), all);
}

#[test]
fn a_message_with_media_is_never_taken_for_short() {
    let image = MediaPart {
        kind: MediaKind::Image,
        media_type: "image/png".into(),
        data: "AAAA".into(),
        name: "shot.png".into(),
    };
    let messages = [
        Message::user("look"),
        Message::User(UserMessage {
            content: "here".into(),
            media: vec![image],
        }),
    ];
    let facts = caches_from(100_000);
    let shape = RequestShape::new("m", &[], "", &messages).with_facts(&facts);
    let asked = CachePlan::none()
        .with_message(0, FiveMinutes)
        .with_history(FiveMinutes);
    let (plan, fixes) = asked.checked(&shape);
    assert_eq!(plan, CachePlan::none().with_history(FiveMinutes));
    assert_eq!(dropped_short(&fixes), [Breakpoint::Message(0)]);
}

#[test]
fn past_four_breakpoints_the_oldest_chosen_messages_go() {
    let (tools, messages) = ([tool()], talk(6));
    let asked = CachePlan::default()
        .with_message(3, FiveMinutes)
        .with_message(1, FiveMinutes)
        .with_message(2, FiveMinutes);
    let (plan, fixes) = asked.checked(&RequestShape::new("m", &tools, "sys", &messages));
    assert_eq!(plan.breakpoints(), MAX_BREAKPOINTS);
    assert_eq!(plan.messages, [(3, FiveMinutes)]);
    let dropped = vec![Breakpoint::Message(1), Breakpoint::Message(2)];
    assert_eq!(fixes, [PlanFix::TooMany { dropped }]);
    assert!(fixes[0].is_warning());
}

#[test]
fn a_shorter_ttl_before_a_longer_one_is_raised_to_it() {
    let (tools, messages) = ([tool()], talk(3));
    let asked = CachePlan::standard(FiveMinutes).with_message(1, OneHour);
    let (plan, fixes) = asked.checked(&RequestShape::new("m", &tools, "sys", &messages));
    assert_eq!(
        (plan.tools, plan.system, plan.messages, plan.history),
        (
            Some(OneHour),
            Some(OneHour),
            vec![(1, OneHour)],
            Some(FiveMinutes)
        )
    );
    let at = vec![Breakpoint::Tools, Breakpoint::System];
    assert_eq!(fixes, [PlanFix::Raised { at }]);
    assert!(!fixes[0].is_warning());
}

impl CachePlan {
    fn without_tools(mut self) -> Self {
        self.tools = None;
        self
    }
}

/// Every model's cache facts are these.
struct Facts(CacheFacts);

impl ModelFacts for Facts {
    fn cache_facts(&self, _spec: &str) -> Option<CacheFacts> {
        Some(self.0.clone())
    }
}

/// The diagnostic log's lines.
#[derive(Default)]
struct Log(Mutex<Vec<String>>);

impl ToolServices for Log {
    fn debug(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

#[tokio::test]
async fn a_fix_is_told_once_to_the_human_or_to_the_log() {
    let p = ScriptedProvider::new([text_answer("a"), text_answer("b")]);
    let hooks = Arc::new(TestHooks::default());
    let log = Arc::new(Log::default());
    // No hour, and nothing under 4,096 tokens: the hour-long system prompt
    // is lowered (the human hears it), then dropped (the log hears it).
    let facts = Facts(CacheFacts::new().with_min_cacheable_tokens(4_096));
    let shared = SharedDeps::new(p.resolver(), "/", ShutWorkspace)
        .with_hooks(hooks.clone())
        .with_services(log.clone())
        .with_facts(facts);
    let opts =
        AgentOptions::new("scripted:m").with_cache_policy(CachePlan::none().with_system(OneHour));
    let mut agent = Agent::new(AgentDeps::new(shared, Tools::new(), "sys"), opts);
    collect(&mut agent, "one").await;
    collect(&mut agent, "two").await;
    let sent: Vec<_> = p.requests().into_iter().map(|r| r.cache).collect();
    assert_eq!(sent, [CachePlan::none(), CachePlan::none()]);
    let warnings = hooks.warnings.lock().unwrap().clone();
    let no_hour = PlanFix::NoHour {
        at: vec![Breakpoint::System],
    };
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains(&no_hour.to_string()), "{warnings:?}");
    assert!(warnings[0].contains("scripted:m"), "names the model");
    let logged = log.0.lock().unwrap().clone();
    let below = PlanFix::BelowMinimum {
        at: Breakpoint::System,
        at_most: p.requests()[0].system.len() as u64,
        minimum: 4_096,
    };
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(logged[0].contains(&below.to_string()), "{logged:?}");
}

/// A policy that notes how many messages each request it plans carries.
#[derive(Default)]
struct Counting(Mutex<Vec<usize>>);

impl CachePolicy for Counting {
    fn plan(&self, request: &RequestShape<'_>) -> CachePlan {
        self.0.lock().unwrap().push(request.messages.len());
        CachePlan::none().with_system(OneHour).with_history(OneHour)
    }
}

#[tokio::test]
async fn the_policy_plans_the_loop_the_pre_warm_and_the_summary() {
    let mut ping = AssistantMessage::start("scripted", "m1");
    ping.usage.cache_read = 100;
    ping.stop_reason = StopReason::MaxTokens;
    let p = ScriptedProvider::new([text_answer("done"), ping, text_answer("THE SUMMARY")]);
    let (mut agent, _hooks) = agent_with(&p, Opts::default());
    let policy = Arc::new(Counting::default());
    agent.set_cache_policy(policy.clone());
    collect(&mut agent, "go").await;
    agent.warm_cache(&CancellationToken::new()).await.unwrap();
    let cfg = CompactionOptions {
        context_window: 1,
        threshold: 0.0,
        ceiling: None,
        keep_recent: 1,
        instructions: None,
    };
    let compacted = compact(&mut agent, &CancellationToken::new(), &cfg).await;
    assert!(matches!(compacted, Ok(Some(_))), "{compacted:?}");
    // The turn's call and its pre-warm send "go"; the summary "go", "done"
    // and its instruction.
    assert_eq!(*policy.0.lock().unwrap(), [1, 1, 3]);
    let planned = CachePlan::none().with_system(OneHour).with_history(OneHour);
    let sent: Vec<_> = p.requests().into_iter().map(|r| r.cache).collect();
    assert_eq!(sent, [planned.clone(), planned.clone(), planned]);
}
