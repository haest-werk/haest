//! A live set's own promises: one set across clones, places kept, enabled
//! state kept. What the loop makes of a change is in `live_loop`.

use super::*;
use crate::agent::test_support::FnTool;

/// A tool called `name` that answers `text`.
pub(super) fn answering(name: &str, text: &'static str) -> ToolRef {
    FnTool::arc(name, false, move |_, _| async move {
        Ok(ToolOutput::text(text))
    })
}

pub(super) fn names(tools: &Tools) -> Vec<String> {
    tools.iter().map(|t| t.name().to_string()).collect()
}

fn abc() -> LiveTools {
    LiveTools::from(Tools::from(vec![
        answering("A", "a"),
        answering("B", "b"),
        answering("C", "c"),
    ]))
}

#[test]
fn every_clone_is_the_one_set() {
    let live = LiveTools::new();
    let agents = live.clone();
    live.set_tool(answering("A", "a"));
    assert_eq!(names(&agents.tools()), ["A"]);
    agents.set_tool(answering("B", "b"));
    assert!(agents.set_enabled("A", false));
    assert_eq!(live.names(), ["A", "B"]);
    assert_eq!(names(&live.tools()), ["B"]);
}

#[test]
fn a_replacement_takes_the_place_of_the_tool_it_replaces() {
    let live = abc();
    let new_b = ToolRef::from(ToolOverride::new(answering("B", "b2")).with_description("new"));
    live.set_tool(new_b.clone());
    live.set_tool(answering("D", "d"));
    assert_eq!(names(&live.tools()), ["A", "B", "C", "D"]);
    assert!(ToolRef::ptr_eq(&live.tools()[1], &new_b));
    assert_eq!(live.tools()[1].description(), "new");
}

#[test]
fn a_disabled_tool_is_neither_advertised_nor_callable_and_comes_back_as_it_was() {
    let live = abc();
    let b = live.tools()[1].clone();
    assert!(live.set_enabled("B", false));
    assert!(!live.is_enabled("B"));
    assert_eq!(names(&live.tools()), ["A", "C"]);
    assert_eq!(names(&live.callable()), ["A", "C"]);
    assert_eq!(live.names(), ["A", "B", "C"], "it keeps its place");
    assert!(live.set_enabled("B", true));
    assert_eq!(names(&live.tools()), ["A", "B", "C"]);
    assert!(ToolRef::ptr_eq(&live.callable()[1], &b), "the same tool");
}

#[test]
fn a_removed_tool_is_gone_and_the_others_keep_their_order() {
    let live = abc();
    assert!(live.remove_tool("B"));
    assert!(!live.remove_tool("B"), "no longer there");
    assert_eq!(live.names(), ["A", "C"]);
    assert_eq!(names(&live.callable()), ["A", "C"]);
    live.set_tool(answering("B", "b"));
    assert_eq!(
        live.names(),
        ["A", "C", "B"],
        "added again, it is a new tool"
    );
}

#[test]
fn a_name_the_set_does_not_hold_changes_nothing() {
    let live = abc();
    assert!(!live.set_enabled("Nope", false));
    assert!(!live.is_enabled("Nope"));
    assert!(!live.remove_tool("Nope"));
    assert_eq!(live.names(), ["A", "B", "C"]);
}

#[test]
fn a_replacement_keeps_the_enabled_state_of_the_tool_it_replaces() {
    let live = abc();
    live.set_enabled("B", false);
    let new_b = answering("B", "b2");
    live.set_tool(new_b.clone());
    assert!(!live.is_enabled("B"));
    assert_eq!(names(&live.tools()), ["A", "C"]);
    live.set_enabled("B", true);
    assert!(
        ToolRef::ptr_eq(&live.tools()[1], &new_b),
        "back as the new one"
    );
}

#[test]
fn a_reload_keeps_the_enabled_state_of_the_names_that_stay_and_forgets_the_rest() {
    let live = abc();
    live.set_enabled("B", false);
    live.set_enabled("C", false);
    live.set_tools(vec![
        answering("D", "d"),
        answering("B", "b2"),
        answering("A", "a2"),
    ]);
    assert_eq!(live.names(), ["D", "B", "A"], "the reload's order");
    assert!(!live.is_enabled("B"), "B stayed, disabled");
    assert!(live.is_enabled("D") && live.is_enabled("A"));
    assert_eq!(names(&live.tools()), ["D", "A"]);
    // C went, so a reload that brings it back brings it enabled.
    live.set_tools(vec![answering("C", "c")]);
    assert!(live.is_enabled("C"));
    assert_eq!(names(&live.tools()), ["C"]);
}

#[test]
fn a_reload_keeps_one_tool_per_name_the_later_one() {
    let live = LiveTools::new();
    let later = answering("A", "a2");
    live.set_tools(vec![
        answering("A", "a1"),
        answering("B", "b"),
        later.clone(),
    ]);
    assert_eq!(live.names(), ["A", "B"]);
    assert!(ToolRef::ptr_eq(&live.tools()[0], &later));
}
