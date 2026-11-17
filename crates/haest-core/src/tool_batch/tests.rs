use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::json;

use super::*;
use crate::{
    agent::test_support::*,
    events::LoopEvent,
    messages::Message,
    testing::{ScriptedProvider, tool_answer},
    tool::{ToolOutput, ToolRef, Tools, opt_num},
};

/// How many calls of a tool run at once, the most there ever were, and the room each saw.
#[derive(Default)]
struct Gauge {
    now: u32,
    max: u32,
    rooms: Vec<u64>,
}

/// A tool that records the gauge and finishes after `ms` (its input).
fn timed_tool(name: &str, read_only: bool, gauge: Arc<Mutex<Gauge>>) -> ToolRef {
    let label = name.to_string();
    FnTool::arc(name, read_only, move |input, ctx| {
        let (gauge, label) = (gauge.clone(), label.clone());
        async move {
            let ms = opt_num(&input, "ms")?.unwrap_or(0.0) as u64;
            {
                let mut g = gauge.lock().unwrap();
                g.now += 1;
                g.max = g.max.max(g.now);
                if let Some(room) = ctx.context_room_tokens {
                    g.rooms.push(room);
                }
            }
            tokio::time::sleep(Duration::from_millis(ms)).await;
            gauge.lock().unwrap().now -= 1;
            Ok(ToolOutput::text(format!("{label}:{ms}")))
        }
    })
}

fn call(id: &str, name: &str, ms: u64) -> (String, String, serde_json::Value) {
    (id.into(), name.into(), json!({"ms": ms}))
}

fn answer(calls: &[(String, String, serde_json::Value)]) -> crate::messages::AssistantMessage {
    let refs: Vec<(&str, &str, serde_json::Value)> = calls
        .iter()
        .map(|(i, n, v)| (i.as_str(), n.as_str(), v.clone()))
        .collect();
    tool_answer(&refs)
}

fn gauge() -> Arc<Mutex<Gauge>> {
    Arc::new(Mutex::new(Gauge::default()))
}

#[test]
fn consecutive_read_only_calls_share_a_batch_and_a_write_or_an_unknown_tool_runs_alone() {
    let g = gauge();
    let tools = ToolIndex::new(Tools::from(vec![
        timed_tool("R", true, g.clone()),
        timed_tool("W", false, g),
    ]));
    let msg = answer(&[
        call("1", "R", 0),
        call("2", "R", 0),
        call("3", "W", 0),
        call("4", "R", 0),
        call("5", "Nope", 0),
        call("6", "R", 0),
    ]);
    let calls: Vec<ToolCall> = msg.tool_calls().cloned().collect();
    let ids: Vec<Vec<&str>> = tool_batches(&calls, &tools)
        .iter()
        .map(|b| b.iter().map(|c| c.id.as_str()).collect())
        .collect();
    assert_eq!(
        ids,
        [vec!["1", "2"], vec!["3"], vec!["4"], vec!["5"], vec!["6"]]
    );
}

#[tokio::test(start_paused = true)]
async fn read_only_calls_overlap_and_results_land_in_call_order_even_when_a_later_one_finishes_first()
 {
    let g = gauge();
    let p = ScriptedProvider::new([answer(&[call("a", "Slow", 60), call("b", "Slow", 5)])]);
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            tools: Some(vec![timed_tool("Slow", true, g.clone())]),
            ..Default::default()
        },
    );
    let events = collect(&mut agent, "go").await;
    assert_eq!(g.lock().unwrap().max, 2);
    let order: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::ToolResult { call, .. } => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["a", "b"]);
    let history: Vec<&str> = agent
        .history()
        .iter()
        .filter_map(|m| match m {
            Message::Tool(t) => Some(t.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(history, ["a", "b"]);
}

#[tokio::test(start_paused = true)]
async fn an_answer_lands_as_soon_as_every_call_before_it_has_answered() {
    let g = gauge();
    let p = ScriptedProvider::new([answer(&[call("a", "Slow", 5), call("b", "Slow", 60)])]);
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            tools: Some(vec![timed_tool("Slow", true, g.clone())]),
            ..Default::default()
        },
    );
    let mut running_at_first_answer = None;
    let mut sink = |ev: LoopEvent| {
        if matches!(&ev, LoopEvent::ToolResult { call, .. } if call.id == "a") {
            running_at_first_answer = Some(g.lock().unwrap().now);
        }
    };
    let _ = agent.run("go", &mut sink).await;
    assert_eq!(running_at_first_answer, Some(1));
}

#[tokio::test(start_paused = true)]
async fn a_write_is_a_barrier_and_nothing_runs_beside_it() {
    let g = gauge();
    let p = ScriptedProvider::new([answer(&[call("a", "W", 20), call("b", "W", 20)])]);
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            tools: Some(vec![timed_tool("W", false, g.clone())]),
            ..Default::default()
        },
    );
    collect(&mut agent, "go").await;
    assert_eq!(g.lock().unwrap().max, 1);
}

async fn rooms(calls: &[(String, String, serde_json::Value)]) -> Vec<u64> {
    let g = gauge();
    let p = ScriptedProvider::new([answer(calls)]);
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            tools: Some(vec![timed_tool("S", true, g.clone())]),
            ..capped(|l| l.max_request_tokens = 100_000)
        },
    );
    collect(&mut agent, "go").await;
    std::mem::take(&mut g.lock().unwrap().rooms)
}

#[tokio::test(start_paused = true)]
async fn a_parallel_batch_splits_the_context_room_so_together_its_results_still_fit() {
    let solo = rooms(&[call("a", "S", 0)]).await;
    let pair = rooms(&[call("a", "S", 0), call("b", "S", 0)]).await;
    assert_eq!(solo.len(), 1);
    assert_eq!(pair.len(), 2);
    assert!(pair[0] + pair[1] <= solo[0]);
}
