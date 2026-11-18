use std::sync::{Arc, Barrier, Mutex, OnceLock};

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

/// Two changes at once (main and a background subagent share one board):
/// the first one's `on_change` is slow (a host writing to disk), and the
/// second change lands while it runs. `on_change` used to hear the newer
/// list first and the older one last, so the last `tasks` record of the
/// session, which replay restores, lost the second task.
#[test]
fn the_last_list_on_change_hears_is_the_board_whatever_the_threads_did() {
    let first_in_hook = Arc::new(Barrier::new(2));
    let second_done = Arc::new(Barrier::new(2));
    let last_heard = Arc::new(Mutex::new(None::<usize>));
    let (in_hook, done, heard) = (
        first_in_hook.clone(),
        second_done.clone(),
        last_heard.clone(),
    );
    let board = TaskBoard::new(move |tasks| {
        if tasks.len() == 1 {
            // The first change's hook: the second change happens meanwhile.
            in_hook.wait();
            done.wait();
        }
        *heard.lock().unwrap() = Some(tasks.len());
    });
    let main = board.clone();
    let first = std::thread::spawn(move || main.create(new("from main")).map(|_| ()));
    first_in_hook.wait();
    board.create(new("from a subagent")).unwrap();
    second_done.wait();
    first.join().expect("the first change panicked").unwrap();
    assert_eq!(board.list().len(), 2);
    assert_eq!(
        *last_heard.lock().unwrap(),
        Some(2),
        "on_change heard an older list last"
    );
}

/// `on_change` may change the board it hears from: that change is heard
/// once the call that made it returns, in order.
#[test]
fn on_change_may_change_the_board() {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let itself = Arc::new(OnceLock::<TaskBoard>::new());
    let (log, me) = (heard.clone(), itself.clone());
    let board = TaskBoard::new(move |tasks| {
        log.lock().unwrap().push(tasks.len());
        if tasks.len() == 1 {
            let b = me.get().expect("set before the first change");
            b.create(new("follow-up")).unwrap();
        }
    });
    let _ = itself.set(board.clone());
    board.create(new("first")).unwrap();
    assert_eq!(*heard.lock().unwrap(), [1, 2]);
    assert_eq!(board.list().len(), 2);
}

/// A clock whose drop sets the board's clock again (a host's paused clock
/// handing over to the wall clock as it goes).
struct SetsClockOnDrop(TaskBoard);

impl crate::deps::Clock for SetsClockOnDrop {
    fn now_ms(&self) -> u64 {
        7
    }
}

impl Drop for SetsClockOnDrop {
    fn drop(&mut self) {
        self.0.set_clock(|| 0);
    }
}

/// The board held the last handle to the old clock, and dropped it while it
/// held the clock lock: the drop's `set_clock` waited for that lock for good.
#[test]
fn a_clock_set_clock_lets_go_of_may_call_the_board_back() {
    let board = TaskBoard::default();
    board.set_clock(SetsClockOnDrop(board.clone()));
    let (done, finished) = std::sync::mpsc::channel();
    let setting = board.clone();
    std::thread::spawn(move || {
        setting.set_clock(|| 1);
        let _ = done.send(());
    });
    let returned = finished.recv_timeout(std::time::Duration::from_secs(5));
    assert!(
        returned.is_ok(),
        "set_clock never returned: the old clock's drop ran under the board's lock"
    );
}
