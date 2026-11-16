//! What the human sees of a subagent: its start, its tools, its guard notices
//! and its end, under its name; nothing of its streaming.

use super::*;
use crate::render::tests::read_result;

fn started() -> SubagentEvent {
    SubagentEvent::Started {
        name: "scout-1".into(),
        parent: "main".into(),
        description: "find the bug".into(),
    }
}

#[test]
fn a_subagents_start_tools_and_end_each_read_as_a_line_under_its_name() {
    let mut log = AgentLog::default();
    let id = "kern-scout-1-abc";
    let lines: Vec<_> = [
        started(),
        SubagentEvent::Loop(LoopEvent::TextDelta { text: "hm".into() }),
        SubagentEvent::Loop(read_result(false)),
        SubagentEvent::Loop(LoopEvent::Guard {
            text: "half the budget".into(),
        }),
        SubagentEvent::Stopped { ok: true },
    ]
    .iter()
    .filter_map(|e| log.event(id, e))
    .collect();
    let levels: Vec<Level> = lines.iter().map(|(l, _)| *l).collect();
    assert_eq!(
        levels,
        [Level::Info, Level::Info, Level::Warning, Level::Info],
        "the text delta shows nothing"
    );
    assert!(
        lines
            .iter()
            .all(|(_, t)| t.contains("scout-1") && !t.contains(id))
    );
}

#[test]
fn an_agent_that_stopped_early_is_a_warning() {
    let mut log = AgentLog::default();
    log.event("a", &started());
    let (level, _) = log
        .event("a", &SubagentEvent::Stopped { ok: false })
        .unwrap();
    assert_eq!(level, Level::Warning);
}

#[test]
fn the_running_count_is_said_only_when_it_changes() {
    let mut log = AgentLog::default();
    assert!(log.activity(0).is_none());
    assert!(log.activity(1).is_some());
    assert!(log.activity(1).is_none());
    assert!(log.activity(0).is_some());
}
