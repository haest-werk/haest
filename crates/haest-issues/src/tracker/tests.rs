use super::*;

#[test]
fn every_relation_inverts_twice_to_itself() {
    for &r in Relation::ALL {
        assert_eq!(r.inverse().inverse(), r);
    }
}

#[test]
fn a_label_patch_replaces_then_adds_then_removes() {
    let current = vec!["a".to_string(), "b".to_string()];
    let add = IssuePatch {
        add_labels: vec!["c".into(), "a".into()],
        remove_labels: vec!["b".into()],
        ..Default::default()
    };
    assert_eq!(add.labels_from(&current), ["a", "c"]);
    let replace = IssuePatch {
        labels: Some(vec!["x".into()]),
        add_labels: vec!["y".into()],
        ..Default::default()
    };
    assert_eq!(replace.labels_from(&current), ["x", "y"]);
    assert!(!IssuePatch::default().changes_labels());
    assert!(IssuePatch::default().is_empty());
}

#[tokio::test]
async fn the_default_filter_lists_the_open_issues() {
    let board = crate::cards::CardTracker::new("/cards", haest_core::fs::MemFs::new());
    let new = NewIssue {
        title: "Fix the login".into(),
        ..Default::default()
    };
    board.create(new).await.unwrap();
    let listed = board.list(&IssueFilter::default()).await.unwrap();
    assert_eq!(listed.len(), 1);
}

#[test]
fn a_value_outside_the_list_names_the_list() {
    let allowed = vec!["low".to_string(), "high".to_string()];
    assert!(one_of("low", &allowed, "priority").is_ok());
    let Err(TrackerError::Invalid(why)) = one_of("urgent", &allowed, "priority") else {
        panic!("an unknown value must be refused");
    };
    assert!(why.contains("low") && why.contains("high"));
    assert!(matches!(
        one_of("high", &[], "priority"),
        Err(TrackerError::Invalid(_))
    ));
}
