//! What the system says of a directory, asked of what is there by the time
//! it is asked.

use std::{fs, process::Command, sync::mpsc, thread, time::Duration};

use crate::spelling::disks::flag;

/// A FIFO put where a directory was, between the lookup's `lstat` and the
/// flag's read: opened to read, it waited for a writer, and held the lookup
/// and a blocking thread until one came, if one ever did.
#[test]
fn a_fifo_where_a_directory_was_is_no_directory_and_no_wait() {
    let t = tempfile::tempdir().unwrap();
    let fifo = t.path().join("lib");
    let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success(), "mkfifo");
    let (tx, rx) = mpsc::channel();
    let asked = fifo.clone();
    let reader = thread::spawn(move || tx.send(flag(&asked)).unwrap());
    let got = rx.recv_timeout(Duration::from_secs(2));
    if got.is_err() {
        // A writer lets the read go, and the test end.
        let _ = fs::OpenOptions::new().write(true).open(&fifo);
    }
    reader.join().unwrap();
    assert_eq!(got.ok(), Some(None), "the flag's read waited on the FIFO");
}
