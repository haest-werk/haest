mod live;
mod live_loop;
mod wrap;

use super::*;
use async_trait::async_trait;
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
    assert!(opt_bool(&i, "b").unwrap());
    assert!(!opt_bool(&i, "e").unwrap(), "\"\" is a flag left out");
    assert!(!opt_bool(&i, "missing").unwrap());
    let flags = map(json!({"t": "True", "f": " false ", "yes": "yes", "one": 1}));
    assert!(opt_bool(&flags, "t").unwrap());
    assert!(!opt_bool(&flags, "f").unwrap());
    for not_a_flag in ["yes", "one"] {
        assert!(opt_bool(&flags, not_a_flag).is_err(), "{not_a_flag}");
    }
    assert!(opt_num(&map(json!({"n": "x"})), "n").is_err());
}

#[test]
fn a_count_is_whole_held_at_the_top_and_never_negative() {
    let i = map(json!({"n": "20.9", "huge": 1e30, "zero": 0, "neg": -1, "e": ""}));
    assert_eq!(opt_count(&i, "n").unwrap(), Some(20));
    assert_eq!(opt_count(&i, "huge").unwrap(), Some(u64::MAX));
    assert_eq!(opt_count(&i, "zero").unwrap(), Some(0));
    assert_eq!(opt_count(&i, "e").unwrap(), None);
    assert_eq!(opt_count(&i, "missing").unwrap(), None);
    assert!(matches!(opt_count(&i, "neg"), Err(ToolError::Input(_))));
    assert_eq!(req_num(&i, "n").unwrap(), 20.9);
    for absent in ["e", "missing"] {
        assert!(matches!(req_num(&i, absent), Err(ToolError::Input(_))));
    }
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

/// An odd `max` keeps `max / 2` at each end, one less than `max` in all: the
/// count said read one short (`len - max`) of what was dropped.
#[test]
fn truncate_middle_counts_every_dropped_character_for_an_odd_max() {
    let (t, cut) = truncate_middle(&"x".repeat(100), 11);
    assert!(cut);
    let kept = t.chars().filter(|c| *c == 'x').count();
    let said: usize = t
        .split('[')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no count in {t:?}"));
    assert_eq!((kept, kept + said), (10, 100), "{t}");
}

/// A tool called `name`, described as `description`.
struct Named(&'static str, &'static str);

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> String {
        self.1.into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "test.named"
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(self.1))
    }
}

fn listed(tools: &Tools) -> Vec<(&str, String)> {
    tools.iter().map(|t| (t.name(), t.description())).collect()
}

/// A host overrides a built-in by adding its own under the same name: the
/// list holds one tool per name, the later one, in the first one's place, so
/// the model is never sent the name twice.
#[test]
fn a_tool_added_under_a_name_already_listed_replaces_it_in_its_place() {
    let want = [("Read", "host's".to_string()), ("Write", "built-in".into())];
    let pushed = Tools::new()
        .with(Named("Read", "built-in"))
        .with(Named("Write", "built-in"))
        .with(Named("Read", "host's"));
    assert_eq!(listed(&pushed), want);

    let mut extended = Tools::new().with(Named("Read", "built-in"));
    extended.extend([Named("Write", "built-in"), Named("Read", "host's")]);
    assert_eq!(listed(&extended), want);

    let collected: Tools = [
        Named("Read", "built-in"),
        Named("Write", "built-in"),
        Named("Read", "host's"),
    ]
    .into_iter()
    .collect();
    assert_eq!(listed(&collected), want);

    let from_vec = Tools::from(vec![
        ToolRef::new(Named("Read", "built-in")),
        ToolRef::new(Named("Write", "built-in")),
        ToolRef::new(Named("Read", "host's")),
    ]);
    assert_eq!(listed(&from_vec), want);
}

#[test]
fn a_tool_is_found_and_removed_by_name() {
    let mut tools = Tools::new()
        .with(Named("Read", "r"))
        .with_all([Named("Write", "w"), Named("Edit", "e")]);
    assert!(tools.contains("Write"));
    assert_eq!(tools.get("Edit").map(|t| t.description()), Some("e".into()));
    assert!(tools.get("Bash").is_none());

    let kept = tools.clone();
    let removed = tools.remove("Write").map(|t| t.description());
    assert_eq!(removed, Some("w".into()));
    assert!(!tools.contains("Write"));
    assert_eq!(listed(&tools), [("Read", "r".into()), ("Edit", "e".into())]);
    assert!(tools.remove("Write").is_none());
    // A clone made before is its own list.
    assert!(kept.contains("Write"));
}

/// A note added to every error keeps each error's kind: the model still
/// gets a bad call back verbatim, and a failure still names the tool.
#[test]
fn a_mapped_error_keeps_its_kind_and_says_the_new_message() {
    let note = |m: String| format!("{m} (see the notebook)");
    let input = ToolError::Input("bad path".into()).map_message(note);
    let failed = ToolError::Failed("disk full".into()).map_message(note);
    assert!(matches!(&input, ToolError::Input(m) if m == "bad path (see the notebook)"));
    assert!(matches!(&failed, ToolError::Failed(m) if m == "disk full (see the notebook)"));
    assert_eq!(failed.message(), "disk full (see the notebook)");
}
