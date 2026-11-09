//! reasoning_details across a Chat Completions stream.

use kern_core::messages::AssistantMessage;
use serde_json::{Value, json};

use super::*;

fn details(deltas: &[Value]) -> Vec<Value> {
    let mut st = StreamState::default();
    let mut msg = AssistantMessage::start("p", "m");
    for d in deltas {
        apply_chunk(
            &json!({"choices": [{"index": 0, "delta": d}]}),
            &mut msg,
            &mut st,
            &mut Vec::new(),
        );
    }
    finish_message(&mut msg, st, false);
    msg.provider_data.unwrap()["reasoning_details"]
        .as_array()
        .unwrap()
        .clone()
}

// OpenRouter streams reasoning_details in per-token FRAGMENTS sharing one
// index. kern kept every fragment as its own item and replayed hundreds of
// them; the model read its own thoughts back one word per line and started
// writing that way, a little worse every turn.
#[test]
fn fragments_of_one_index_merge_into_one_item_with_the_whole_text() {
    let mut deltas: Vec<Value> = ["The ", "queue ", "sink."]
        .iter()
        .map(|t| json!({"reasoning": t, "reasoning_details": [{"type": "reasoning.text", "text": t, "index": 0}]}))
        .collect();
    deltas.push(
        json!({"reasoning_details": [{"type": "reasoning.text", "signature": "sig", "index": 0}]}),
    );
    assert_eq!(
        details(&deltas),
        vec![
            json!({"type": "reasoning.text", "text": "The queue sink.", "signature": "sig", "index": 0})
        ]
    );
}

#[test]
fn different_indexes_stay_separate_items_in_index_order() {
    let got = details(&[
        json!({"reasoning_details": [{"type": "reasoning.encrypted", "data": "x", "index": 1}]}),
        json!({"reasoning_details": [{"type": "reasoning.text", "text": "a", "index": 0}]}),
    ]);
    let indexes: Vec<_> = got.iter().map(|d| d["index"].clone()).collect();
    assert_eq!(indexes, [json!(0), json!(1)]);
}

#[test]
fn reasoning_content_is_thinking_and_also_kept_apart_for_replay() {
    let mut st = StreamState::default();
    let mut msg = AssistantMessage::start("p", "m");
    let mut out = Vec::new();
    for d in [
        json!({"reasoning_content": "a"}),
        json!({"reasoning_content": "b"}),
    ] {
        apply_chunk(
            &json!({"choices": [{"index": 0, "delta": d}]}),
            &mut msg,
            &mut st,
            &mut out,
        );
    }
    finish_message(&mut msg, st, false);
    assert_eq!(out.len(), 2);
    assert_eq!(msg.provider_data.unwrap()["reasoning_content"], json!("ab"));
}
