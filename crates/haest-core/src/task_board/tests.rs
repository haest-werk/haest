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
    let a = b.create(new("schema")).unwrap();
    let c = b.create(new("api")).unwrap();
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
    })
    .unwrap();
    let patch = TaskPatch {
        status: Some(PatchStatus::Set(TaskStatus::InProgress)),
        ..Default::default()
    };
    b.update("1", patch).unwrap();
    let t = b.get("1").unwrap();
    assert_eq!(t.subject, "keep me");
    assert_eq!(t.description.as_deref(), Some("details"));
    assert_eq!(t.status, TaskStatus::InProgress);
}

#[test]
fn an_unknown_id_is_refused_before_anything_changes() {
    let b = TaskBoard::default();
    b.create(new("a")).unwrap();
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
    b.create(new("a")).unwrap();
    let patch = TaskPatch {
        status: Some(PatchStatus::Set(TaskStatus::Completed)),
        add_blocked_by: vec!["1".into()],
        ..Default::default()
    };
    assert!(b.update("1", patch).is_err());
    assert_eq!(b.get("1").unwrap().status, TaskStatus::Pending);
    assert_eq!(*snapshots.lock().unwrap(), 1);
}

#[test]
fn a_restored_list_continues_the_id_count() {
    let b = TaskBoard::default();
    b.restore(vec![saved("7")]);
    assert_eq!(b.create(new("y")).unwrap().id, "8");
}

#[test]
fn completing_stamps_completed_at_from_the_board_clock_every_clone_reads() {
    let host = TaskBoard::default();
    // The tools' clone is made before the host sets the clock, and reads it.
    let b = host.clone();
    host.set_clock(|| 42);
    b.create(new("a")).unwrap();
    let patch = TaskPatch {
        status: Some(PatchStatus::Set(TaskStatus::Completed)),
        ..Default::default()
    };
    let t = b.update("1", patch).unwrap().unwrap();
    assert_eq!((t.updated_at, t.completed_at), (42, Some(42)));
}

#[test]
fn every_clone_is_the_same_board() {
    let host = TaskBoard::default();
    let tools = host.clone();
    tools.create(new("a")).unwrap();
    assert_eq!(host.list().len(), 1);
    assert_eq!(
        host.create(new("b")).unwrap().id,
        "2",
        "one count across clones"
    );
}

/// A saved task with this id and nothing else set.
fn saved(id: &str) -> Task {
    Task {
        id: id.into(),
        subject: "saved".into(),
        description: None,
        active_form: None,
        status: TaskStatus::Pending,
        blocked_by: vec![],
        blocks: vec![],
        owner: None,
        updated_at: 0,
        completed_at: None,
    }
}

/// A restored id near u64::MAX (a hand-edited session file) leaves room for
/// one more task at most: no id is handed out twice.
#[test]
fn ids_stay_unique_when_the_count_runs_out() {
    let b = TaskBoard::default();
    b.restore(vec![saved(&(u64::MAX - 1).to_string())]);
    let last = b.create(new("last")).unwrap();
    assert_eq!(last.id, u64::MAX.to_string());
    assert_eq!(b.create(new("one too many")), Err(TaskError::IdsExhausted));
    let ids: Vec<String> = b.list().into_iter().map(|t| t.id).collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "an id was handed out twice: {ids:?}"
    );
    assert_eq!(ids.len(), 2, "the refused create added nothing");

    // Restored at the very top: nothing is left to hand out.
    let full = TaskBoard::default();
    full.restore(vec![saved(&u64::MAX.to_string())]);
    assert_eq!(full.create(new("y")), Err(TaskError::IdsExhausted));
    assert_eq!(full.list().len(), 1);
}
