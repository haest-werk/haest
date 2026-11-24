//! A host runs a build, or a listing, on a task of its own: `tokio::spawn`
//! and a protocol library's task queue take only `Send` futures. Each check
//! here fails to compile when one is not.

use std::{future::Future, path::Path, sync::Arc};

use haest_core::fs::MemFs;

use crate::{
    BuildOptions, Resume, RuntimeInputs, World, build, list_sessions,
    testkit::{Deny, Recorded, settings},
};

fn send<F: Future + Send>(_: F) {}

#[test]
fn a_build_and_a_listing_can_run_on_a_task_of_their_own() {
    let inputs = RuntimeInputs::new(settings(), World::files(MemFs::new()));
    let opts = BuildOptions {
        resume: Resume::Latest,
        ..BuildOptions::default()
    };
    send(build(inputs, Recorded::default(), Deny, opts));

    let fs = Arc::new(MemFs::new());
    send(list_sessions(fs.as_ref(), Path::new("/data/sessions")));
}
