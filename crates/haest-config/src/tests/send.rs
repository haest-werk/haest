//! A host loads the config on a task of its own: `tokio::spawn` and a
//! protocol library's task queue take only `Send` futures. This fails to
//! compile when the load is not one.

use std::future::Future;

use super::*;

fn send<F: Future + Send>(_: F) {}

#[test]
fn a_load_can_run_on_a_task_of_its_own() {
    let fs = MemFs::new();
    send(Loader::standard(Settings::bare("/w", HOME), vars(&[]), owned(&[])).load(&fs));
}
