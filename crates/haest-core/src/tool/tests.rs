use super::*;
use serde_json::json;

fn map(v: Value) -> JsonMap {
    v.as_object().unwrap().clone()
}

#[test]
fn input_helpers_accept_what_models_send() {
    let i = map(json!({"a": "x", "n": "42", "b": "true", "e": "", "blank": "  "}));
    assert_eq!(req_str(&i, "a").unwrap(), "x");
    assert!(req_str(&i, "e").is_err());
    assert_eq!(opt_str(&i, "e").unwrap(), None);
    assert_eq!(opt_num(&i, "n").unwrap(), Some(42.0));
    // "" for a number is no number.
    assert_eq!(opt_num(&i, "e").unwrap(), None);
    assert_eq!(opt_num(&i, "blank").unwrap(), None);
    assert!(opt_bool(&i, "b"));
    assert!(opt_num(&map(json!({"n": "x"})), "n").is_err());
}

#[test]
fn a_string_is_spelled_as_itself_and_anything_else_as_its_json() {
    assert_eq!(spelled(&json!("a b.png")), "a b.png");
    for v in [
        json!(80),
        json!(1.5),
        json!(true),
        json!(null),
        json!(["a"]),
    ] {
        let back: Value = serde_json::from_str(&spelled(&v)).unwrap();
        assert_eq!(back, v);
    }
}

#[test]
fn truncate_middle_keeps_both_ends() {
    let (t, cut) = truncate_middle("abcdefghij", 4);
    assert!(cut);
    assert!(t.starts_with("ab") && t.ends_with("ij") && t.contains("6 characters truncated"));
    assert_eq!(truncate_middle("short", 10), ("short".to_string(), false));
}

#[test]
fn a_title_is_the_hosts_and_absent_unless_set() {
    let plain = serde_json::to_value(ToolOutput::text("sent")).unwrap();
    assert!(plain.get("title").is_none());
    let titled = ToolOutput::text("sent").with_title("Notified you: done");
    assert_eq!(
        serde_json::to_value(&titled).unwrap()["title"],
        "Notified you: done"
    );
    assert_eq!(titled.content, "sent");
}
