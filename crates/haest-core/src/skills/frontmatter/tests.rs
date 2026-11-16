use super::*;

#[test]
fn the_flat_reader_skips_blocks_and_unquotes() {
    let meta = flat_meta(&[
        "name: \"x: y\\n\\x41\"",
        "nested:",
        "  child: 1",
        "list: [a, 'b', ]",
        "empty:",
    ]);
    assert_eq!(meta["name"], Meta::Str("x: y\nA".into()));
    assert!(!meta.contains_key("nested") && !meta.contains_key("child"));
    assert_eq!(meta["list"], Meta::List(vec!["a".into(), "b".into()]));
    assert_eq!(meta["empty"], Meta::Str(String::new()));
}

/// `levels` anchors, each a list of nine aliases of the one before.
fn alias_bomb(levels: usize) -> String {
    let mut s = String::from("name: bomb\nl0: &l0 [x, x, x, x, x, x, x, x, x]\n");
    for i in 1..levels {
        let prev = vec![format!("*l{}", i - 1); 9].join(", ");
        s += &format!("l{i}: &l{i} [{prev}]\n");
    }
    s
}

/// Nine levels would be ~9^9 nodes if expanded: refused before any is built.
#[test]
fn a_frontmatter_whose_aliases_blow_up_is_refused() {
    for levels in [6, 9] {
        let bomb = alias_bomb(levels);
        let head: Vec<&str> = bomb.lines().collect();
        assert_eq!(read_meta(&head), None, "{levels} levels");
    }
}

#[test]
fn ampersands_stars_and_a_small_alias_still_read() {
    let meta = read_meta(&[
        "name: r-and-d",
        "description: Use *this* & that, R&D only",
        "argument-hint: \"*.rs & *.toml\"",
        "tools: &t [Read, Grep]",
        "allowed-tools: *t",
    ])
    .unwrap();
    assert_eq!(
        meta["description"],
        Meta::Str("Use *this* & that, R&D only".into())
    );
    assert_eq!(meta["argument-hint"], Meta::Str("*.rs & *.toml".into()));
    let tools = Meta::List(vec!["Read".into(), "Grep".into()]);
    assert_eq!((&meta["tools"], &meta["allowed-tools"]), (&tools, &tools));
}
