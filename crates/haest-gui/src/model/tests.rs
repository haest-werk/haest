//! The reduction, frame by frame: what each view gets from a stream.

use haest::core::{
    LoopEvent,
    messages::{
        AssistantMessage, AssistantPart, Message, StopReason, ToolCall, ToolResultMessage, Usage,
        UserMessage,
    },
    tool::ToolOutput,
};
use haest::session::{Body, Frame};
use haest::wire::{Access, Answer, Answerer, Envelope, Outbound, Path, Snapshot, State, WIRE_V};
use serde_json::json;

use super::*;

/// A stream frame: `seq`, at `ts`, about `path`.
fn frame(seq: u64, ts: u64, path: Path, body: impl Into<Body>) -> Frame {
    Envelope {
        v: WIRE_V,
        sid: "s1".into(),
        seq,
        ts,
        path,
        body: body.into(),
    }
}

fn main_path() -> Path {
    Path::main()
}

fn user(text: &str) -> LoopEvent {
    LoopEvent::User {
        message: UserMessage {
            content: text.into(),
            media: Vec::new(),
        },
    }
}

fn model_start() -> LoopEvent {
    LoopEvent::ModelStart {
        provider: "p".into(),
        model: "m".into(),
    }
}

fn call(id: &str, name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
    }
}

fn answer(parts: Vec<AssistantPart>, usage: Usage) -> LoopEvent {
    let mut message = AssistantMessage::start("p", "m");
    message.content = parts;
    message.usage = usage;
    if message.tool_calls().next().is_some() {
        message.stop_reason = StopReason::ToolUse;
    }
    LoopEvent::Assistant { message }
}

fn text(t: &str) -> AssistantPart {
    AssistantPart::Text { text: t.into() }
}

fn thinking(t: &str) -> AssistantPart {
    AssistantPart::Thinking {
        thinking: t.into(),
        signature: None,
        redacted: None,
    }
}

fn result(c: &ToolCall, content: &str, error: bool, ms: u64) -> LoopEvent {
    let mut output = ToolOutput::text(content);
    output.is_error = error;
    LoopEvent::ToolResult {
        call: c.clone(),
        message: ToolResultMessage {
            tool_call_id: c.id.clone(),
            tool_name: c.name.clone(),
            content: content.into(),
            is_error: error,
            media: Vec::new(),
        },
        output,
        duration_ms: ms,
    }
}

fn turn_end() -> LoopEvent {
    LoopEvent::TurnEnd {
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        duration_ms: 0,
        error: None,
    }
}

fn usage(input: u64, read: u64, write: u64) -> Usage {
    Usage {
        input,
        output: 10,
        cache_read: read,
        cache_write: write,
        ..Usage::default()
    }
}

fn kinds(s: &Stream) -> Vec<&ItemKind> {
    s.items.iter().map(|i| &i.kind).collect()
}

fn session() -> SessionModel {
    let mut s = SessionModel::new("s1", Access::Command);
    s.conn = Conn::Live;
    s
}

/// Applies `events` on main, seq 1.. and ts 1000 + 100 per frame.
fn feed(s: &mut SessionModel, events: Vec<LoopEvent>) {
    for ev in events {
        let seq = s.last_seq + 1;
        s.apply(&frame(seq, 1000 + 100 * seq, main_path(), ev));
    }
}

// ---- stream ----

#[test]
fn deltas_stream_into_a_live_answer_that_the_assistant_frame_replaces() {
    let mut s = session();
    feed(
        &mut s,
        vec![
            user("hi"),
            model_start(),
            LoopEvent::ThinkingDelta { text: "hm".into() },
            LoopEvent::TextDelta { text: "Hel".into() },
            LoopEvent::TextDelta { text: "lo".into() },
        ],
    );
    let main = s.stream(MAIN).unwrap();
    assert!(main.streaming);
    assert_eq!(
        kinds(main),
        [
            &ItemKind::User { text: "hi".into() },
            &ItemKind::Thinking {
                text: "hm".into(),
                live: false
            },
            &ItemKind::Answer {
                text: "Hello".into(),
                live: true
            },
        ]
    );
    feed(
        &mut s,
        vec![answer(
            vec![thinking("hmm."), text("Hello!")],
            Usage::default(),
        )],
    );
    let main = s.stream(MAIN).unwrap();
    assert!(!main.streaming);
    assert_eq!(
        kinds(main)[1..],
        [
            &ItemKind::Thinking {
                text: "hmm.".into(),
                live: false
            },
            &ItemKind::Answer {
                text: "Hello!".into(),
                live: false
            },
        ]
    );
}

/// A late peer gets a call's `assistant` without its deltas (the ring
/// dropped them): the answer is whole all the same.
#[test]
fn an_answer_without_its_deltas_is_whole() {
    let mut s = session();
    feed(
        &mut s,
        vec![
            user("hi"),
            model_start(),
            answer(vec![text("Done.")], Usage::default()),
        ],
    );
    assert_eq!(
        kinds(s.stream(MAIN).unwrap())[1],
        &ItemKind::Answer {
            text: "Done.".into(),
            live: false
        }
    );
}

#[test]
fn a_tool_card_gets_its_input_preview_then_its_result() {
    let mut s = session();
    let bash = call("c1", "Bash", json!({"command": "ls -la\n/tmp"}));
    feed(
        &mut s,
        vec![
            user("list"),
            model_start(),
            LoopEvent::ToolCallStart {
                id: "c1".into(),
                name: "Bash".into(),
            },
            answer(
                vec![AssistantPart::ToolCall(bash.clone())],
                Usage::default(),
            ),
        ],
    );
    let card = |s: &SessionModel| match &s.stream(MAIN).unwrap().items.last().unwrap().kind {
        ItemKind::Tool(c) => c.clone(),
        other => panic!("not a tool card: {other:?}"),
    };
    let running = card(&s);
    assert_eq!(running.preview, "ls -la /tmp");
    assert_eq!(running.status, ToolStatus::Running);
    feed(&mut s, vec![result(&bash, "total 0", true, 42)]);
    let done = card(&s);
    assert_eq!(
        done.status,
        ToolStatus::Done {
            ms: Some(42),
            error: true
        }
    );
    assert_eq!(done.result.as_deref(), Some("total 0"));
}

#[test]
fn an_item_changes_its_rev_but_keeps_its_uid_as_it_grows() {
    let mut s = session();
    feed(
        &mut s,
        vec![model_start(), LoopEvent::TextDelta { text: "a".into() }],
    );
    let before = s.stream(MAIN).unwrap().items[0].clone();
    feed(&mut s, vec![LoopEvent::TextDelta { text: "b".into() }]);
    let after = &s.stream(MAIN).unwrap().items[0];
    assert_eq!(after.uid, before.uid);
    assert!(after.rev > before.rev);
}

#[test]
fn a_frame_already_applied_is_skipped() {
    let mut s = session();
    assert!(s.apply(&frame(5, 1, main_path(), user("once"))));
    assert!(!s.apply(&frame(5, 1, main_path(), user("once"))));
    assert!(!s.apply(&frame(3, 1, main_path(), user("older"))));
    assert_eq!(s.stream(MAIN).unwrap().items.len(), 1);
}

#[test]
fn input_previews_say_what_a_call_does_on_one_line() {
    use super::stream::{input_preview, one_line};
    let input = |v: serde_json::Value| v.as_object().cloned().unwrap();
    assert_eq!(
        input_preview("Read", &input(json!({"file_path": "/a/b.rs"}))),
        "/a/b.rs"
    );
    assert_eq!(input_preview("Todo", &input(json!({"n": 1}))), r#"{"n":1}"#);
    assert_eq!(input_preview("X", &input(json!({}))), "");
    assert_eq!(one_line("abcdef", 4), "abc…");
    assert_eq!(one_line("a\nb", 10), "a b");
}

// ---- timeline ----

#[test]
fn a_model_call_bar_spans_model_start_to_assistant_and_carries_its_cache_split() {
    let mut s = session();
    s.apply(&frame(1, 1000, main_path(), user("go")));
    s.apply(&frame(2, 1100, main_path(), model_start()));
    s.apply(&frame(
        3,
        1600,
        main_path(),
        answer(vec![text("ok")], usage(100, 600, 300)),
    ));
    let turn = &s.timelines[MAIN].turns[0];
    assert_eq!(turn.start_ts, 1000);
    let c = &turn.calls[0];
    assert_eq!((c.start_ts, c.end_ts), (1100, Some(1600)));
    let split = CacheSplit::of(c.usage.as_ref().unwrap()).unwrap();
    assert!((split.served - 0.6).abs() < 1e-6);
    assert!((split.written - 0.3).abs() < 1e-6);
    assert!((split.uncached - 0.1).abs() < 1e-6);
    assert_eq!(CacheSplit::of(&Usage::default()), None);
}

#[test]
fn the_calls_of_one_batch_stack_one_lane_each_and_end_at_their_answers() {
    let mut s = session();
    let calls: Vec<_> = (0..3)
        .map(|i| call(&format!("c{i}"), "Read", json!({"path": i})))
        .collect();
    let parts = calls.iter().cloned().map(AssistantPart::ToolCall).collect();
    s.apply(&frame(1, 1000, main_path(), user("read three")));
    s.apply(&frame(2, 1010, main_path(), model_start()));
    s.apply(&frame(
        3,
        1200,
        main_path(),
        answer(parts, Usage::default()),
    ));
    let turn = &s.timelines[MAIN].turns[0];
    assert_eq!(turn.lanes(), 4);
    assert!(
        turn.tools
            .iter()
            .all(|t| t.batch == 0 && t.end_ts.is_none())
    );
    assert_eq!(
        turn.tools.iter().map(|t| t.lane).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    // Each answer: its batch started `duration` before it.
    s.apply(&frame(
        4,
        1500,
        main_path(),
        result(&calls[0], "a", false, 300),
    ));
    s.apply(&frame(
        5,
        1700,
        main_path(),
        result(&calls[1], "b", true, 500),
    ));
    let turn = &s.timelines[MAIN].turns[0];
    assert_eq!(
        (turn.tools[0].start_ts, turn.tools[0].end_ts),
        (1200, Some(1500))
    );
    assert_eq!(
        (turn.tools[1].start_ts, turn.tools[1].end_ts),
        (1200, Some(1700))
    );
    assert!(turn.tools[1].error);
    // The turn ends: what still ran ends with it.
    s.apply(&frame(6, 1800, main_path(), turn_end()));
    let turn = &s.timelines[MAIN].turns[0];
    assert_eq!(turn.tools[2].end_ts, Some(1800));
    assert_eq!(turn.end_ts, Some(1800));
    assert!(!s.timelines[MAIN].is_running());
}

#[test]
fn steering_input_does_not_open_a_second_turn() {
    let mut s = session();
    feed(
        &mut s,
        vec![
            user("a"),
            model_start(),
            user("also b"),
            turn_end(),
            user("c"),
        ],
    );
    let turns = &s.timelines[MAIN].turns;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].number, 2);
    assert!(turns[1].end_ts.is_none());
}

// ---- agents ----

#[test]
fn subagents_build_a_tree_from_their_parents_names() {
    let mut s = session();
    let started = |name: &str, parent: &str| Outbound::SubagentStarted {
        name: name.into(),
        parent: parent.into(),
        description: format!("{name}'s job"),
    };
    s.apply(&frame(
        1,
        10,
        Path::subagent("a1"),
        started("reader", "main"),
    ));
    s.apply(&frame(
        2,
        20,
        Path::subagent("b2"),
        started("digger", "reader"),
    ));
    s.apply(&frame(3, 30, Path::subagent("b2"), model_start()));
    assert!(s.tree.get("b2").unwrap().streaming);
    s.apply(&frame(
        4,
        40,
        Path::subagent("b2"),
        LoopEvent::TextDelta {
            text: "found".into(),
        },
    ));
    s.apply(&frame(
        5,
        50,
        Path::subagent("b2"),
        Outbound::SubagentStopped { ok: false },
    ));
    s.apply(&frame(
        6,
        60,
        Path::subagent("a1"),
        Outbound::SubagentStopped { ok: true },
    ));

    let flat: Vec<_> = s
        .tree
        .flatten()
        .into_iter()
        .map(|(d, n)| (d, n.id.as_str(), n.state))
        .collect();
    assert_eq!(
        flat,
        [
            (0, "main", AgentState::Running),
            (1, "a1", AgentState::Finished),
            (2, "b2", AgentState::StoppedEarly),
        ]
    );
    assert!(!s.tree.get("b2").unwrap().streaming);
    // Each agent's frames are its own stream; its parent's says it started and stopped.
    assert_eq!(
        kinds(s.stream("b2").unwrap()),
        [&ItemKind::Answer {
            text: "found".into(),
            live: true
        }]
    );
    let in_reader: Vec<_> = kinds(s.stream("a1").unwrap())
        .into_iter()
        .filter_map(|k| match k {
            ItemKind::Agent { id, phase, .. } => Some((id.as_str(), *phase)),
            _ => None,
        })
        .collect();
    // One row per subagent: its phase moves on, no second row.
    assert_eq!(in_reader, [("b2", AgentPhase::StoppedEarly)]);
    assert!(
        kinds(s.stream(MAIN).unwrap())
            .iter()
            .any(|k| matches!(k, ItemKind::Agent { id, .. } if id == "a1"))
    );
}

#[test]
fn an_unknown_parent_hangs_under_main() {
    let mut tree = AgentTree::new();
    tree.started("x1", "orphan", "nobody", "", 0);
    assert_eq!(tree.get("x1").unwrap().parent.as_deref(), Some(MAIN));
    assert_eq!(tree.subagents(), 1);
    assert_eq!(tree.running(), 1);
}

// ---- approvals ----

fn ask(n: u64, tool: &str) -> Outbound {
    Outbound::ApprovalAsk {
        ask: n,
        tool_name: tool.into(),
        tool_call_id: Some(format!("c{n}")),
        description: format!("run {tool}"),
        preview: "rm -rf build".into(),
        key: Some("rm -rf build".into()),
    }
}

fn answered(n: u64, answer: Answer, by: Answerer) -> Outbound {
    Outbound::ApprovalAnswered { ask: n, answer, by }
}

#[test]
fn asks_from_every_session_queue_oldest_first() {
    let mut a = session();
    let mut b = SessionModel::new("s2", Access::Command);
    b.conn = Conn::Live;
    a.apply(&frame(1, 300, main_path(), ask(1, "Bash")));
    b.apply(&frame(1, 100, main_path(), ask(1, "Write")));
    a.apply(&frame(2, 200, main_path(), ask(2, "Edit")));
    let queue: Vec<_> = approval_queue([&a, &b])
        .into_iter()
        .map(|(s, ask)| (s.sid.as_str(), ask.tool_name.as_str()))
        .collect();
    assert_eq!(queue, [("s2", "Write"), ("s1", "Edit"), ("s1", "Bash")]);
}

#[test]
fn a_withdrawn_ask_leaves_the_queue() {
    let mut s = session();
    s.apply(&frame(1, 1, main_path(), ask(7, "Bash")));
    s.apply(&frame(2, 2, main_path(), ask(8, "Bash")));
    s.apply(&frame(
        3,
        3,
        main_path(),
        Outbound::ApprovalWithdrawn { ask: 7 },
    ));
    let left: Vec<_> = s.asks.iter().map(|a| a.id).collect();
    assert_eq!(left, [AskId::Approval(8)]);
}

#[test]
fn an_answer_is_marked_until_confirmed_then_gone() {
    let mut s = session();
    s.apply(&frame(1, 1, main_path(), ask(3, "Bash")));
    s.answering(AskId::Approval(3), Answer::Always);
    assert_eq!(s.asks[0].answering, Some(Answer::Always));
    s.answered(AskId::Approval(3));
    assert!(s.asks.is_empty());
}

/// Answered at the haest's own terminal, or by another program: the wire
/// says so, and the ask leaves at once, with a line saying who answered.
#[test]
fn an_ask_answered_elsewhere_leaves_at_once() {
    let mut s = session();
    s.apply(&frame(1, 1, main_path(), ask(1, "Bash")));
    s.apply(&frame(2, 2, main_path(), ask(2, "Write")));
    s.apply(&frame(
        3,
        3,
        main_path(),
        answered(1, Answer::Deny, Answerer::Terminal),
    ));
    assert_eq!(
        s.asks.iter().map(|a| a.id).collect::<Vec<_>>(),
        [AskId::Approval(2)]
    );
    s.apply(&frame(
        4,
        4,
        main_path(),
        answered(2, Answer::Allow, Answerer::Peer),
    ));
    assert!(s.asks.is_empty());
    let lines: Vec<String> = kinds(s.stream(MAIN).unwrap())
        .into_iter()
        .filter_map(|k| match k {
            ItemKind::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        lines.contains(&"Bash: denied at its terminal".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Write: allowed by another program".to_string()),
        "{lines:?}"
    );
}

/// The window's own answer comes back as a peer's: the line says it was answered here.
#[test]
fn its_own_answer_is_said_as_answered_here() {
    let mut s = session();
    s.apply(&frame(1, 1, main_path(), ask(1, "Bash")));
    s.answering(AskId::Approval(1), Answer::Always);
    s.apply(&frame(
        2,
        2,
        main_path(),
        answered(1, Answer::Always, Answerer::Peer),
    ));
    assert!(s.asks.is_empty());
    let said = kinds(s.stream(MAIN).unwrap())
        .into_iter()
        .any(|k| matches!(k, ItemKind::Notice { text, .. } if text == "Bash: allowed always here"));
    assert!(said);
}

/// An approval ask stays past main's turn end (a subagent's may outlive
/// it): only the wire ends it.
#[test]
fn an_approval_ask_ends_only_when_the_wire_says() {
    let mut s = session();
    s.apply(&frame(1, 1, main_path(), user("go")));
    s.apply(&frame(2, 2, main_path(), ask(1, "Bash")));
    s.apply(&frame(3, 3, main_path(), turn_end()));
    assert_eq!(s.asks.len(), 1);
    s.apply(&frame(
        4,
        4,
        main_path(),
        Outbound::ApprovalWithdrawn { ask: 1 },
    ));
    assert!(s.asks.is_empty());
}

/// The terminal's question on an approval ask is that same ask: one entry,
/// answerable here, marked as asked at the terminal too.
#[test]
fn the_terminals_side_of_an_approval_ask_is_the_same_ask() {
    let mut s = session();
    s.apply(&frame(1, 1, main_path(), ask(4, "Bash")));
    let twin = Outbound::Ask {
        text: "Allow Bash: rm -rf build? [y]es / [a]lways / [N]o: ".into(),
        approval: Some(4),
    };
    s.apply(&frame(2, 2, main_path(), twin));
    assert_eq!(s.asks.len(), 1);
    assert!(s.answerable(&s.asks[0]) && s.asks[0].at_terminal);
}

/// The ask names the call it holds up: that card, and no other, waits for it.
#[test]
fn an_ask_holds_the_call_it_names() {
    let mut s = session();
    s.apply(&frame(1, 1, main_path(), ask(7, "Bash")));
    assert!(s.holds("c7"));
    assert!(!s.holds("c8"), "another Bash call is not held");
    s.answering(AskId::Approval(7), Answer::Deny);
    assert!(
        !s.holds("c7"),
        "answered here: no longer waiting on a human"
    );
}

#[test]
fn a_terminal_ask_is_shown_not_answerable_and_leaves_when_a_tool_answers() {
    let mut s = session();
    let c = call("c1", "Bash", json!({"command": "make"}));
    let question = Outbound::Ask {
        text: "Allow Bash: make?".into(),
        approval: None,
    };
    s.apply(&frame(1, 1, main_path(), question));
    assert_eq!(s.asks.len(), 1);
    assert!(!s.answerable(&s.asks[0]));
    s.apply(&frame(2, 2, main_path(), result(&c, "ok", false, 5)));
    assert!(s.asks.is_empty());
}

#[test]
fn a_watch_socket_cannot_answer() {
    let mut s = SessionModel::new("s1", Access::Watch);
    s.conn = Conn::Live;
    s.apply(&frame(1, 1, main_path(), ask(1, "Bash")));
    assert!(!s.answerable(&s.asks[0]));
}

// ---- gaps ----

#[test]
fn a_gap_marks_the_stream_and_history_rebuilds_it_then_frames_resume_after_its_seq() {
    let mut s = session();
    feed(&mut s, vec![user("first"), model_start()]);
    let gap = Envelope::connection(
        "s1",
        2,
        Body::Wire(Outbound::Gap {
            after: 2,
            missed: Some(9),
        }),
    );
    assert!(s.apply(&gap));
    assert_eq!(s.gap, Some(Some(9)));
    assert!(matches!(
        kinds(s.stream(MAIN).unwrap()).last(),
        Some(ItemKind::Lost {
            missed: Some(9),
            caught_up: false
        })
    ));
    assert!(s.timelines[MAIN].turns[0].partial);

    let c = call("c1", "Read", json!({"path": "/x"}));
    let mut a = AssistantMessage::start("p", "m");
    a.content = vec![text("reading"), AssistantPart::ToolCall(c.clone())];
    let messages = vec![
        Message::user("first"),
        Message::Assistant(a),
        Message::Tool(ToolResultMessage {
            tool_call_id: "c1".into(),
            tool_name: "Read".into(),
            content: "data".into(),
            is_error: false,
            media: Vec::new(),
        }),
    ];
    s.apply_history(11, &messages);
    assert_eq!(s.gap, None);
    let main = s.stream(MAIN).unwrap();
    assert!(matches!(&main.items[2].kind, ItemKind::Tool(card)
        if card.preview == "/x"
        && card.status == ToolStatus::Done { ms: None, error: false }
        && card.result.as_deref() == Some("data")));
    assert!(matches!(
        main.items.last().unwrap().kind,
        ItemKind::Lost {
            missed: Some(9),
            caught_up: true
        }
    ));
    // Frames the history already holds are skipped; later ones apply.
    let before = s.stream(MAIN).unwrap().items.len();
    assert!(!s.apply(&frame(10, 5, main_path(), user("old"))));
    assert!(!s.apply(&frame(11, 5, main_path(), user("old"))));
    assert!(s.apply(&frame(12, 5, main_path(), user("new"))));
    assert_eq!(s.stream(MAIN).unwrap().items.len(), before + 1);
}

#[test]
fn status_frames_keep_the_header_current() {
    let mut s = session();
    let now = Snapshot {
        state: State::Running,
        session: "sess".into(),
        model_spec: "anthropic:claude-haiku-5-5".into(),
        effort: Some("low".into()),
        usd: Some(0.0123),
        queued: 1,
        messages: 4,
        last_seq: 9,
    };
    s.apply(&frame(1, 1, main_path(), Outbound::Status(now)));
    assert_eq!(s.state, State::Running);
    assert_eq!(s.model, "anthropic:claude-haiku-5-5");
    assert_eq!(s.effort.as_deref(), Some("low"));
    assert_eq!((s.queued, s.messages, s.usd), (1, 4, Some(0.0123)));
}

#[test]
fn a_short_id_is_eight_chars() {
    assert_eq!(short("0123456789abcdef"), "01234567");
    assert_eq!(short("abc"), "abc");
}

// Found in the critique: haest's "Subagents running: N" notices repeat what
// the subagent's own row and the Agents tab already say.
#[test]
fn subagent_count_notices_are_left_out_other_notices_stay() {
    let mut s = session();
    let note = |text: &str| Outbound::Notice {
        level: haest::wire::Level::Info,
        text: text.into(),
    };
    s.apply(&frame(1, 10, main_path(), note("Subagents running: 1")));
    s.apply(&frame(2, 20, main_path(), note("Subagents running: 0")));
    s.apply(&frame(
        3,
        30,
        main_path(),
        note("Compacted the conversation"),
    ));
    let texts: Vec<_> = kinds(s.stream(MAIN).unwrap())
        .into_iter()
        .filter_map(|k| match k {
            ItemKind::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["Compacted the conversation"]);
}
