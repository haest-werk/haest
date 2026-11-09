use std::sync::Arc;

use super::*;

fn new(subject: &str) -> NewTask {
    NewTask {
        subject: subject.into(),
        ..Default::default()
    }
}

#[test]
fn ids_count_up_a_dependency_is_kept_on_both_ends_and_deleting_unlinks() {
    let b = TaskBoard::default();
    let a = b.create(new("schema"));
    let c = b.create(new("api"));
    let patch = TaskPatch {
        add_blocked_by: vec![a.id.clone()],
        ..Default::default()
    };
    b.update(&c.id, patch).unwrap();
    assert_eq!([a.id.as_str(), c.id.as_str()], ["1", "2"]);
    assert_eq!(b.get("1").unwrap().blocks, ["2"]);
    assert_eq!(b.get("2").unwrap().blocked_by, ["1"]);
    let del = TaskPatch {
        status: Some(PatchStatus::Deleted),
        ..Default::default()
    };
    assert_eq!(b.update("1", del).unwrap(), None);
    let ids: Vec<String> = b.list().into_iter().map(|t| t.id).collect();
    assert_eq!(ids, ["2"]);
    assert!(b.get("2").unwrap().blocked_by.is_empty());
}

#[test]
fn an_update_only_touches_the_fields_it_names() {
    let b = TaskBoard::default();
    b.create(NewTask {
        subject: "keep me".into(),
        description: Some("details".into()),
        active_form: None,
    });
    let patch = TaskPatch {
        status: Some(PatchStatus::Set(KernTaskStatus::InProgress)),
        ..Default::default()
    };
    b.update("1", patch).unwrap();
    let t = b.get("1").unwrap();
    assert_eq!(t.subject, "keep me");
    assert_eq!(t.description.as_deref(), Some("details"));
    assert_eq!(t.status, KernTaskStatus::InProgress);
}

#[test]
fn an_unknown_id_is_refused_before_anything_changes() {
    let b = TaskBoard::default();
    b.create(new("a"));
    let patch = TaskPatch {
        add_blocked_by: vec!["9".into()],
        ..Default::default()
    };
    assert!(b.update("1", patch).is_err());
    assert!(b.get("1").unwrap().blocked_by.is_empty());
}

#[test]
fn a_task_blocking_itself_is_refused_before_anything_changes() {
    let snapshots = Arc::new(Mutex::new(0));
    let seen = snapshots.clone();
    let b = TaskBoard::new(move |_| *seen.lock().unwrap() += 1);
    b.create(new("a"));
    let patch = TaskPatch {
        status: Some(PatchStatus::Set(KernTaskStatus::Completed)),
        add_blocked_by: vec!["1".into()],
        ..Default::default()
    };
    assert!(b.update("1", patch).is_err());
    assert_eq!(b.get("1").unwrap().status, KernTaskStatus::Pending);
    assert_eq!(*snapshots.lock().unwrap(), 1);
}

#[test]
fn a_restored_list_continues_the_id_count() {
    let b = TaskBoard::default();
    b.restore(vec![KernTask {
        id: "7".into(),
        subject: "x".into(),
        description: None,
        active_form: None,
        status: KernTaskStatus::Pending,
        blocked_by: vec![],
        blocks: vec![],
        owner: None,
        updated_at: 0,
        completed_at: None,
    }]);
    assert_eq!(b.create(new("y")).id, "8");
}

#[test]
fn completing_stamps_completed_at_from_the_board_clock() {
    let b = TaskBoard::with_clock(|_| {}, || 42);
    b.create(new("a"));
    let patch = TaskPatch {
        status: Some(PatchStatus::Set(KernTaskStatus::Completed)),
        ..Default::default()
    };
    let t = b.update("1", patch).unwrap().unwrap();
    assert_eq!((t.updated_at, t.completed_at), (42, Some(42)));
}
