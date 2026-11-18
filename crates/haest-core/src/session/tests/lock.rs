//! One writer per session: its lock is a file of its own beside the session
//! file, taken before the session file is written.

use super::*;

/// The lock is `<id>.lock` beside the session file, which nothing reads or
/// writes. Where a file system's locks are mandatory (SMB refuses every
/// read and write of a locked file but through the lock's own), a lock on
/// the session file itself refused the writer its own records.
#[tokio::test]
async fn the_lock_is_a_file_beside_the_session_so_the_session_still_writes_and_reads() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    s.try_lock().await.unwrap();
    s.append(&msg(1, "kept"));
    assert_eq!(texts(&s.load().await.unwrap().messages), ["kept"]);
    let asked = fs.asked_to_lock.lock().unwrap().clone();
    assert_eq!(asked, [PathBuf::from("/data/sessions/abc.lock")]);
}

/// One writer per session: a second one's `try_lock` is refused until the
/// first is dropped. The holder taking it again is no second lock. A new
/// session is held before its file exists: holding it writes nothing of the
/// session, and the sessions dir it makes is private.
#[tokio::test]
async fn a_held_session_refuses_another_writer_until_the_holder_drops() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.try_lock().await.unwrap();
    assert!(
        !s.exists().await,
        "holding a new session writes nothing of it"
    );
    let modes = fs.modes.lock().unwrap().clone();
    assert!(modes.contains(&(PathBuf::from("/data/sessions"), 0o700)));
    s.try_lock().await.unwrap();
    s.create("c1", Path::new("/x"), 0).await.unwrap();

    let other = store(&fs);
    let refused = other.try_lock().await.unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    drop(s);
    other.try_lock().await.unwrap();
}

/// Holding a new session makes its dir private (0700), even when the dir
/// is there: the lock file is made in it, and the session's file needs
/// that anyway (a dir that took no new file left the session unlocked). A
/// saved session's dir is left as it is.
#[tokio::test]
async fn holding_a_new_session_makes_its_dir_private_and_a_saved_one_leaves_it_alone() {
    let fs = Arc::new(ModeFs::default());
    fs.create_dir_all(Path::new("/data/sessions"))
        .await
        .unwrap();
    let s = store(&fs);
    s.try_lock().await.unwrap();
    let modes = fs.modes.lock().unwrap().clone();
    assert_eq!(modes, [(PathBuf::from("/data/sessions"), 0o700)]);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    drop(s);

    fs.modes.lock().unwrap().clear();
    store(&fs).try_lock().await.unwrap();
    assert_eq!(*fs.modes.lock().unwrap(), []);
}

/// A file system with no locks holds nothing, refuses nothing, and gets no
/// lock file.
#[tokio::test]
async fn on_a_file_system_with_no_locks_every_writer_goes_on() {
    let fs = Arc::new(MemFs::new());
    let s = JsonlSessionFile::new(fs.clone(), DATA, "abc").unwrap();
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    s.try_lock().await.unwrap();
    let other = JsonlSessionFile::new(fs.clone(), DATA, "abc").unwrap();
    other.try_lock().await.unwrap();
    assert!(!fs.exists(Path::new("/data/sessions/abc.lock")).await);
}
