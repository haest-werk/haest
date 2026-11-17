//! Ported from code.test.ts: promises, the output (Claude Code REPL format), inner
//! calls as transcript rows, and see().

use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use haest_core::{
    ToolRef, Tools,
    messages::{MediaKind, MediaPart},
    tool::{InnerCallEvent, ToolOutput},
};
use serde_json::json;

use super::{arc, builtins, code_over, probe, returned, run, run_in};
use crate::{
    code::{MAX_SEEN, OUTPUT_BEGIN, OUTPUT_END},
    testkit::{MemWorkspace, Probe, Recorder, ctx},
};

#[tokio::test]
async fn promises_inside_the_returned_value_are_awaited_and_the_calls_ran_at_once() {
    let r = probe("mcp__s__read", true, 30);
    let code = "const o = {}
       o.first = tools.mcp__s__read({})
       o.more = [tools.mcp__s__read({}), { deep: tools.mcp__s__read({}) }]
       return o";
    let out = run(&code_over(vec![arc(&r)], &[]), code).await;
    assert_eq!(
        returned(&out),
        Some(json!({"first": "ok", "more": ["ok", {"deep": "ok"}]}))
    );
    assert_eq!(r.peak(), 3);
}

#[tokio::test]
async fn an_inner_result_reaches_the_program_whole_however_big() {
    // Over the loop's spill size: the program, not the context, is what reads it.
    let mut big = Probe::new("mcp__s__big", true, 0);
    big.answer = ToolOutput::text("x".repeat(50_000));
    let out = run(
        &code_over(Tools::from_iter([ToolRef::new(big)]), &[]),
        "const r = await tools.mcp__s__big({}); return r.length",
    )
    .await;
    assert_eq!(returned(&out), Some(json!(50_000)));
}

#[tokio::test]
async fn a_call_nobody_awaited_still_finishes_before_the_result() {
    let ws = MemWorkspace::from_files([("/p/keep.txt", "")]);
    let code = "tools.Write({ file_path: 'late.txt', content: 'landed' }); return 'done'";
    let out = run_in(
        &code_over(builtins(), &[]),
        code,
        json!({}),
        &ctx(ws.clone()),
    )
    .await;
    assert_eq!(returned(&out), Some(json!("done")));
    assert_eq!(ws.contents("/p/late.txt").await.as_deref(), Some("landed"));
}

#[tokio::test]
async fn everything_printed_sits_between_the_markers_and_the_call_count_after_them() {
    let p = probe("mcp__s__read", true, 0);
    let out = run(
        &code_over(vec![arc(&p)], &[]),
        "console.log('hello'); await tools.mcp__s__read({}); return 7",
    )
    .await;
    let lines: Vec<&str> = out.content.split('\n').collect();
    let begin = lines.iter().position(|l| *l == OUTPUT_BEGIN).unwrap();
    let end = lines.iter().position(|l| *l == OUTPUT_END).unwrap();
    assert_eq!(begin, 0);
    assert_eq!(lines[begin + 1..end], ["hello", "→ 7"]);
    // Exactly one line after the fence: the call count.
    assert_eq!(lines.len(), end + 2);
}

#[tokio::test]
async fn an_uncaught_error_names_the_line_in_the_model_code() {
    let out = run(
        &code_over(Tools::new(), &[]),
        "const a = 1\nconst b = 2\nnull.boom()",
    )
    .await;
    assert!(out.is_error);
    let at = out.content.find("(line ").expect("a line number") + "(line ".len();
    let line: String = out.content[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    assert_eq!(line, "3");
}

#[tokio::test]
async fn a_huge_return_value_is_cut_to_fit_one_result_and_never_spilled() {
    let out = run(&code_over(Tools::new(), &[]), "return 'x'.repeat(500000)").await;
    assert!(out.sized);
    assert!(out.content.chars().count() < 25_000);
    assert!(out.content.split('\n').any(|l| l == OUTPUT_END));
}

#[tokio::test]
async fn each_inner_call_reports_start_then_end_tied_to_the_code_call_a_failure_included() {
    let ok = probe("mcp__s__ok", true, 0);
    let mut bad = Probe::new("mcp__s__bad", true, 0);
    bad.answer = ToolOutput::error("nope");
    let tools = vec![arc(&ok), ToolRef::new(bad)];
    let services = Arc::new(Recorder::default());
    let c = ctx(MemWorkspace::from_files::<&str, &str>([]))
        .with_tool_call_id("call-1")
        .with_services(services.clone());
    let code = "await tools.mcp__s__ok({}); try { await tools.mcp__s__bad({}) } catch (e) {}";
    run_in(&code_over(tools, &[]), code, json!({}), &c).await;
    let seen: Vec<(&str, String, String, Option<bool>)> = services
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|ev| match ev {
            InnerCallEvent::Start { parent_id, call } => {
                ("start", parent_id.clone(), call.id.clone(), None)
            }
            InnerCallEvent::End {
                parent_id,
                call,
                output,
                ..
            } => (
                "end",
                parent_id.clone(),
                call.id.clone(),
                Some(output.is_error),
            ),
            other => panic!("an inner call event this test does not know: {other:?}"),
        })
        .collect();
    let want = |phase, id: &str, err| (phase, "call-1".to_string(), id.to_string(), err);
    assert_eq!(
        seen,
        [
            want("start", "call-1.1", None),
            want("end", "call-1.1", Some(false)),
            want("start", "call-1.2", None),
            want("end", "call-1.2", Some(true)),
        ]
    );
}

#[tokio::test]
async fn outside_the_loop_no_inner_call_is_reported() {
    let ok = probe("mcp__s__ok", true, 0);
    let services = Arc::new(Recorder::default());
    let c = ctx(MemWorkspace::from_files::<&str, &str>([])).with_services(services.clone());
    run_in(
        &code_over(vec![arc(&ok)], &[]),
        "await tools.mcp__s__ok({})",
        json!({}),
        &c,
    )
    .await;
    assert!(services.events.lock().unwrap().is_empty());
}

const PNG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0, 0, 0, 0];

#[tokio::test]
async fn see_attaches_the_image_to_the_code_result_as_media_the_model_sees() {
    let ws = MemWorkspace::from_files([("/p/shot.png", PNG)]);
    let code = "await see({ text: 'the login page', images: [{ path: 'shot.png' }] })";
    let out = run_in(&code_over(builtins(), &[]), code, json!({}), &ctx(ws)).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(
        out.media,
        [MediaPart {
            kind: MediaKind::Image,
            media_type: "image/png".into(),
            data: STANDARD.encode(PNG),
            name: "shot.png".into(),
        }]
    );
}

#[tokio::test]
async fn see_stops_at_its_cap_per_run_and_refuses_a_non_image() {
    let mut files: Vec<(String, Vec<u8>)> = (0..=MAX_SEEN)
        .map(|i| (format!("/p/s{i}.png"), PNG.to_vec()))
        .collect();
    files.push(("/p/notes.txt".into(), b"text".to_vec()));
    let ws = MemWorkspace::from_files(files);
    let code = format!(
        "const r = []
       for (let i = 0; i <= {MAX_SEEN}; i++) {{ try {{ await see({{ images: [{{ path: 's' + i + '.png' }}] }}); r.push('ok') }} catch (e) {{ r.push('refused') }} }}
       try {{ await see({{ images: [{{ path: 'notes.txt' }}] }}); r.push('ok') }} catch (e) {{ r.push('refused') }}
       return r"
    );
    let out = run_in(&code_over(builtins(), &[]), &code, json!({}), &ctx(ws)).await;
    let mut want = vec![json!("ok"); MAX_SEEN];
    want.extend([json!("refused"), json!("refused")]);
    assert_eq!(returned(&out), Some(json!(want)));
    assert_eq!(out.media.len(), MAX_SEEN);
}

#[tokio::test]
async fn see_holds_its_cap_when_calls_run_at_once() {
    // A Read that takes a while, as a real one does: both see() calls are in flight.
    let mut read = Probe::new("Read", true, 30);
    read.answer = ToolOutput::text("a picture");
    read.answer.media = vec![MediaPart {
        kind: MediaKind::Image,
        media_type: "image/png".into(),
        data: STANDARD.encode(PNG),
        name: "s.png".into(),
    }];
    let batch = |from: usize| {
        (from..from + MAX_SEEN)
            .map(|i| format!("{{ path: 's{i}.png' }}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let code = format!(
        "const r = await Promise.allSettled([see({{ images: [{}] }}), see({{ images: [{}] }})])
       return r.map(x => x.status)",
        batch(0),
        batch(MAX_SEEN)
    );
    let out = run(
        &code_over(Tools::from_iter([ToolRef::new(read)]), &[]),
        &code,
    )
    .await;
    assert_eq!(out.media.len(), MAX_SEEN, "{}", out.content);
    assert_eq!(returned(&out), Some(json!(["fulfilled", "rejected"])));
}
