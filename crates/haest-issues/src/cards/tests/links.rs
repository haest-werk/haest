//! Relations between cards: stored on one side, read from both.

use super::*;

const CARD: &str = "---\ntitle: x\nstatus: open\n---\n";

/// A board with these cards, each a bare open card.
fn cards(dir: &Path, ids: &[&str]) -> CardTracker {
    for id in ids {
        write_card(dir, id, CARD);
    }
    board(dir)
}

async fn link(b: &CardTracker, id: &str, relation: Relation, target: &str, change: LinkChange) {
    b.link(id, &Link::new(relation, target), change)
        .await
        .unwrap_or_else(|e| panic!("{id} {} {target}: {e}", relation.as_str()));
}

fn relations(issue: &Issue) -> Vec<(&'static str, &str)> {
    (issue.links.iter())
        .map(|l| (l.relation.as_str(), l.target.as_str()))
        .collect()
}

#[tokio::test]
async fn each_relation_is_stored_on_the_side_the_board_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let b = cards(tmp.path(), &["epic", "a", "b", "c", "d"]);
    use LinkChange::Add;
    link(&b, "a", Relation::Parent, "epic", Add).await;
    link(&b, "epic", Relation::Child, "d", Add).await;
    link(&b, "a", Relation::Blocks, "b", Add).await;
    link(&b, "c", Relation::DependsOn, "a", Add).await;
    link(&b, "a", Relation::RelatesTo, "c", Add).await;
    link(&b, "a", Relation::RelatesTo, "c", Add).await;

    let card = |id| read_card(tmp.path(), id);
    assert!(card("a").contains("epic: epic\n"));
    assert!(card("a").contains("relates_to: [c]\n"), "added once");
    assert!(card("d").contains("epic: epic\n"));
    assert!(card("b").contains("depends_on: [a]\n"));
    assert!(card("c").contains("depends_on: [a]\n"));
    assert!(
        !card("epic").contains("epic: "),
        "a child is stored on the child"
    );

    // Each card reads its own links and the ones others hold toward it.
    let a = b.get("a").await.unwrap();
    assert_eq!(
        relations(&a),
        [
            ("parent", "epic"),
            ("relates_to", "c"),
            ("blocks", "b"),
            ("blocks", "c")
        ]
    );
    let epic = b.get("epic").await.unwrap();
    assert_eq!(relations(&epic), [("child", "a"), ("child", "d")]);
    let c = b.get("c").await.unwrap();
    assert_eq!(relations(&c), [("depends_on", "a"), ("relates_to", "a")]);
}

#[tokio::test]
async fn a_new_parent_replaces_the_old_one() {
    let tmp = tempfile::tempdir().unwrap();
    let b = cards(tmp.path(), &["a", "e1", "e2"]);
    link(&b, "a", Relation::Parent, "e1", LinkChange::Add).await;
    link(&b, "e2", Relation::Child, "a", LinkChange::Add).await;
    let a = b.get("a").await.unwrap();
    assert_eq!(relations(&a), [("parent", "e2")]);
}

#[tokio::test]
async fn a_link_comes_off_where_it_is_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let b = cards(tmp.path(), &["epic", "a", "b", "c"]);
    use LinkChange::{Add, Remove};
    link(&b, "a", Relation::Parent, "epic", Add).await;
    link(&b, "a", Relation::DependsOn, "b", Add).await;
    link(&b, "c", Relation::RelatesTo, "a", Add).await;

    link(&b, "epic", Relation::Child, "a", Remove).await;
    link(&b, "b", Relation::Blocks, "a", Remove).await;
    // Stored on c, taken off from a's side: relates_to reads both ways.
    link(&b, "a", Relation::RelatesTo, "c", Remove).await;
    for id in ["a", "c"] {
        assert_eq!(
            read_card(tmp.path(), id),
            format!("{CARD}\n\n"),
            "{id} has no links left"
        );
    }
    let missing = b
        .link("a", &Link::new(Relation::DependsOn, "b"), Remove)
        .await;
    assert!(matches!(missing, Err(TrackerError::NotFound(_))));
}

#[tokio::test]
async fn a_loop_a_self_link_and_a_missing_card_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let b = cards(tmp.path(), &["a", "b", "c"]);
    use LinkChange::Add;
    link(&b, "a", Relation::DependsOn, "b", Add).await;
    link(&b, "b", Relation::DependsOn, "c", Add).await;
    let refuse = |id: &'static str, relation, target: &'static str| {
        let b = &b;
        async move { b.link(id, &Link::new(relation, target), Add).await }
    };
    assert!(matches!(
        refuse("c", Relation::DependsOn, "a").await,
        Err(TrackerError::Invalid(_))
    ));
    assert!(matches!(
        refuse("a", Relation::Blocks, "c").await,
        Err(TrackerError::Invalid(_))
    ));
    link(&b, "a", Relation::Parent, "b", Add).await;
    assert!(matches!(
        refuse("a", Relation::Child, "b").await,
        Err(TrackerError::Invalid(_))
    ));
    assert!(matches!(
        refuse("a", Relation::RelatesTo, "a").await,
        Err(TrackerError::Invalid(_))
    ));
    assert!(matches!(
        refuse("a", Relation::RelatesTo, "nope").await,
        Err(TrackerError::NotFound(_))
    ));
    assert!(!read_card(tmp.path(), "c").contains("depends_on"));
}
