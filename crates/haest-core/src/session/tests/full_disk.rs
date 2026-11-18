//! A disk that fills up mid-session, then has room again: an append that
//! failed costs the record that failed, and no record after it.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// `store`, and how many queued writes have failed so far.
fn counting_store(fs: &Arc<ModeFs>) -> (JsonlSessionFile, Arc<AtomicUsize>) {
    let failed = Arc::new(AtomicUsize::new(0));
    let count = failed.clone();
    let s = JsonlSessionFile::new_with_error_handler(fs.clone(), DATA, "abc", move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    });
    (s.unwrap(), failed)
}

/// A disk that fills up mid-record keeps what fit of it, and the append
/// fails. The first record written once the disk has room again is a line
/// of its own, so it replays: only the record whose failure was reported is
/// lost.
#[tokio::test]
async fn a_record_written_after_a_failed_append_replays() {
    let fs = Arc::new(ModeFs::default());
    let (s, failed) = counting_store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    s.append(&msg(0, "kept"));
    s.flush().await;
    *fs.short_append.lock().unwrap() = Some(20);
    s.append(&msg(1, "cut short"));
    s.append(&msg(2, "go on"));
    s.flush().await;
    assert_eq!(failed.load(Ordering::SeqCst), 1);

    let back = store(&fs).load().await.unwrap();
    assert_eq!(texts(&back.messages), ["kept", "go on"]);
    assert_eq!(back.corrupt_lines, 1, "the record that failed");
}

/// A disk that refuses a write whole keeps nothing of the record: the next
/// one replays, and nothing else is counted lost.
#[tokio::test]
async fn a_record_written_after_an_append_that_wrote_nothing_replays() {
    let fs = Arc::new(ModeFs::default());
    let (s, failed) = counting_store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    s.append(&msg(0, "kept"));
    s.flush().await;
    *fs.short_append.lock().unwrap() = Some(0);
    s.append(&msg(1, "refused"));
    s.append(&msg(2, "go on"));
    s.flush().await;
    assert_eq!(failed.load(Ordering::SeqCst), 1);

    let back = store(&fs).load().await.unwrap();
    assert_eq!(texts(&back.messages), ["kept", "go on"]);
    assert_eq!(back.corrupt_lines, 0);
}
