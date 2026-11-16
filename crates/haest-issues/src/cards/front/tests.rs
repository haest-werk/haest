use super::*;

/// A card the way people and the board write them: a quoted title, a block
/// list, a nested block, a block scalar, keys this crate has never heard of.
const CARD: &str = "---
title: \"Phase 1: the parser\"
gate: strict
status: open
created: 2026-08-19T12:00:00.000Z
refs:
  - docs/plan.md
  - \"notes: one\"
promise:
  closes:
    - a

  owner: someone
priority: high
note: |
  first line
  second line
blocked_by: [b-card]
depends_on: [a-card]
tags: [parser, epic]
empty:
---

# Body

Text.
";

#[test]
fn a_card_reads_its_keys_lists_and_body() {
    let f = Front::parse(CARD).unwrap();
    assert_eq!(f.str("title"), Some("Phase 1: the parser"));
    assert_eq!(f.list("refs"), ["docs/plan.md", "notes: one"]);
    assert_eq!(f.list("tags"), ["parser", "epic"]);
    // The alias is folded into the key it stands for.
    assert_eq!(f.list("depends_on"), ["a-card", "b-card"]);
    assert!(f.get("blocked_by").is_none());
    assert!(matches!(f.get("promise"), Some(Val::Block(_))));
    assert_eq!(f.body, "# Body\n\nText.");
    assert!(Front::parse("# no frontmatter\n").is_none());
}

#[test]
fn a_write_keeps_every_key_and_puts_the_stores_keys_first() {
    let out = Front::parse(CARD).unwrap().render();
    let expected = "---
title: \"Phase 1: the parser\"
status: open
priority: high
tags: [parser, epic]
refs: [docs/plan.md, \"notes: one\"]
depends_on: [a-card, b-card]
created: 2026-08-19T12:00:00.000Z
gate: strict
empty: \"\"
promise:
  closes:
    - a

  owner: someone
note: |
  first line
  second line
---

# Body

Text.
";
    assert_eq!(out, expected);
    // Written once, it is stable: reading and writing again changes nothing.
    assert_eq!(Front::parse(&out).unwrap().render(), out);
}

#[test]
fn every_scalar_reads_back_as_it_was_written() {
    for value in [
        "plain",
        "EPIC: unify",
        "2026-08-15T06:08:44.054Z",
        "ends with:",
        "has # but no comment",
        "a #comment",
        "[not a list]",
        "'quoted'",
        " padded ",
        "two\nlines\twith a tab",
        "bell \u{7} and \"quotes\" and \\",
        "",
    ] {
        let mut f = Front::default();
        f.set("title", Val::Str(value.into()));
        f.set("x", Val::Str(value.into()));
        let line = scalar(value);
        assert!(!line.contains('\n'), "one line: {line:?}");
        let back = Front::parse(&f.render()).unwrap();
        assert_eq!(back.get("x"), Some(&Val::Str(value.into())), "{line}");
    }
    assert_eq!(
        scalar("2026-08-15T06:08:44.054Z"),
        "2026-08-15T06:08:44.054Z"
    );
}

#[test]
fn only_a_plain_block_list_under_a_list_key_becomes_a_list() {
    let head = |block: &str| format!("---\ntitle: t\n{block}\n---\n");
    let mapping = Front::parse(&head("refs:\n  - a: b")).unwrap();
    assert!(matches!(mapping.get("refs"), Some(Val::Block(_))));
    let uneven = Front::parse(&head("refs:\n  - a\n    - b")).unwrap();
    assert!(matches!(uneven.get("refs"), Some(Val::Block(_))));
    let not_a_list_key = Front::parse(&head("steps:\n  - a")).unwrap();
    assert!(matches!(not_a_list_key.get("steps"), Some(Val::Block(_))));
    let quoted = Front::parse(&head("tags:\n  - \"a: b\"\n  - c")).unwrap();
    assert_eq!(quoted.list("tags"), ["a: b", "c"]);
}

#[test]
fn an_empty_list_removes_its_key() {
    let mut f = Front::parse(CARD).unwrap();
    f.set_list("tags", Vec::new());
    assert!(f.get("tags").is_none());
    assert!(!f.render().contains("tags:"));
}
