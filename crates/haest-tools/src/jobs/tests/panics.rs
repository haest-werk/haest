//! A panic while a job runs, in a host's [`JobEvents`] handler or in the
//! workspace: the job is still reported ended.

use std::{
    panic::{self, AssertUnwindSafe},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use haest_core::{
    fs::MemFs,
    tool::ToolContext,
    workspace::{ExecResult, WorkspaceRef},
};
use haest_workspace::{RealWorkspace, SandboxWorkspace};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::Recorder;
use crate::{
    jobs::{Job, JobEvents, JobHub, JobOutput, JobStatus, NewJob},
    testkit::TestWorkspace,
};

/// The sandbox at `/` and the real workspace in `tmp`, by name.
fn workspaces(tmp: &Path) -> [(&'static str, WorkspaceRef, &Path); 2] {
    let sandbox: WorkspaceRef = SandboxWorkspace::over(MemFs::new()).into();
    let real: WorkspaceRef = RealWorkspace::new(std::env::vars().collect()).into();
    [("sandbox", sandbox, Path::new("/")), ("real", real, tmp)]
}

/// The next notice the model is told, within 5 s.
async fn notice(notices: &mut UnboundedReceiver<String>) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .ok()
        .flatten()
}

/// Panics on every chunk of output; hears the rest as [`Recorder`] does.
#[derive(Default)]
struct PanicsOnOutput(Recorder);

impl JobEvents for PanicsOnOutput {
    fn on_change(&self, running: &[Job]) {
        self.0.on_change(running);
    }
    fn on_output(&self, job: &Job, output: JobOutput<'_>) {
        if let JobOutput::Chunk(chunk) = output {
            panic!("a host's handler fails on {chunk:?}");
        }
        self.0.on_output(job, output);
    }
    fn on_exit(&self, job: &Job) {
        self.0.on_exit(job);
    }
}

/// Over the sandbox the handler ran on the job's own task, and its panic
/// skipped the job's end: the job stayed running for good, with no exit
/// code, no Done and no notice. Over the real workspace it ended the pipe's
/// reader, and the job died of SIGPIPE on its next write.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_output_handler_panics_runs_to_its_end_and_is_reported_ended() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, ws, cwd) in workspaces(tmp.path()) {
        let (inbox, mut notices) = unbounded_channel();
        let hub = JobHub::new(inbox);
        let events = Arc::new(PanicsOnOutput::default());
        hub.set_events(events.clone());
        let command = NewJob::new("echo one; sleep 0.2; echo two; exit 3");
        let job = hub.start(command, &ToolContext::new(cwd, ws)).unwrap();
        // The notice is the last thing a job's end does.
        let told = notice(&mut notices).await;

        let ended = hub.get(&job.id).unwrap();
        assert_eq!(
            (ended.status, ended.exit_code),
            (JobStatus::Exited, Some(3)),
            "{name}"
        );
        assert!(hub.running().is_empty(), "{name}");
        assert_eq!(hub.buffer(&job.id).unwrap(), "one\ntwo\n", "{name}");
        // It hears no more of the job's output once it panicked, Done included.
        let done = events.0.chunks.lock().unwrap().iter().any(|row| row.2);
        assert!(!done, "{name}: Done went to a handler that panicked");
        assert_eq!(events.0.exit_count(), 1, "{name}");
        assert!(
            told.is_some_and(|n| n.contains(&job.id)),
            "{name}: the model is told"
        );
    }
}

/// A host whose handler panicked once holding its own lock panics on every
/// later call. Done still went to it after its first panic, and Done's panic
/// skipped the running set, `on_exit` and the notice: the model was never
/// told the job exited.
#[tokio::test(flavor = "multi_thread")]
async fn a_handler_that_panics_on_all_output_still_hears_the_end_and_the_model_is_told() {
    /// Panics on every output call, Done included; counts them.
    #[derive(Default)]
    struct Poisoned {
        heard: Recorder,
        outputs: AtomicUsize,
    }
    impl JobEvents for Poisoned {
        fn on_change(&self, running: &[Job]) {
            self.heard.on_change(running);
        }
        fn on_output(&self, _job: &Job, output: JobOutput<'_>) {
            self.outputs.fetch_add(1, Ordering::SeqCst);
            panic!("a host's handler with a poisoned lock fails on {output:?}");
        }
        fn on_exit(&self, job: &Job) {
            self.heard.on_exit(job);
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    for (name, ws, cwd) in workspaces(tmp.path()) {
        let (inbox, mut notices) = unbounded_channel();
        let hub = JobHub::new(inbox);
        let events = Arc::new(Poisoned::default());
        hub.set_events(events.clone());
        let command = NewJob::new("echo one; sleep 0.2; echo two; exit 3");
        let job = hub.start(command, &ToolContext::new(cwd, ws)).unwrap();
        let told = notice(&mut notices).await;

        assert!(
            told.is_some_and(|n| n.contains(&job.id)),
            "{name}: the model is told"
        );
        let ended = hub.get(&job.id).unwrap();
        assert_eq!(ended.exit_code, Some(3), "{name}");
        assert_eq!(events.heard.exit_count(), 1, "{name}: on_exit");
        let last = events.heard.snapshots.lock().unwrap().last().cloned();
        assert_eq!(last, Some(Vec::new()), "{name}: the last set");
        assert_eq!(
            events.outputs.load(Ordering::SeqCst),
            1,
            "{name}: one call, then no more"
        );
    }
}

/// `start` handed the set to `on_change` before it spawned the job, so a
/// panic there left the job in the table as running, never run: KillShell
/// could not end it, and after [`MAX_JOBS`](crate::jobs::MAX_JOBS) such
/// starts every new job was refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_whose_on_change_panics_still_runs_the_job_to_its_end() {
    /// Panics on every running set; hears the rest as [`Recorder`] does.
    #[derive(Default)]
    struct PanicsOnChange(Recorder);
    impl JobEvents for PanicsOnChange {
        fn on_change(&self, _running: &[Job]) {
            panic!("a host's on_change fails");
        }
        fn on_output(&self, job: &Job, output: JobOutput<'_>) {
            self.0.on_output(job, output);
        }
        fn on_exit(&self, job: &Job) {
            self.0.on_exit(job);
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    for (name, ws, cwd) in workspaces(tmp.path()) {
        let (inbox, mut notices) = unbounded_channel();
        let hub = JobHub::new(inbox);
        let events = Arc::new(PanicsOnChange::default());
        hub.set_events(events.clone());
        let ctx = ToolContext::new(cwd, ws);
        let started = panic::catch_unwind(AssertUnwindSafe(|| {
            hub.start(NewJob::new("echo ran; exit 4"), &ctx)
        }));
        assert!(started.is_err(), "{name}: the host's panic reaches it");
        let told = notice(&mut notices).await;

        assert!(
            told.is_some_and(|n| n.contains("b1")),
            "{name}: the model is told"
        );
        let ended = hub.get("b1").unwrap();
        assert_eq!(
            (ended.status, ended.exit_code),
            (JobStatus::Exited, Some(4)),
            "{name}"
        );
        assert!(hub.running().is_empty(), "{name}");
        assert_eq!(hub.buffer("b1").unwrap(), "ran\n", "{name}");
        assert_eq!(events.0.exit_count(), 1, "{name}: on_exit");
        let done = events.0.chunks.lock().unwrap().iter().any(|row| row.2);
        assert!(done, "{name}: Done");
    }
}

/// A workspace that panics (a host's own, or a host's shell command in the
/// sandbox) ended the job's task before the job's end: it stayed running for
/// good, counting toward the cap.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_workspace_panics_is_reported_ended() {
    let (inbox, mut notices) = unbounded_channel();
    let hub = JobHub::new(inbox);
    let ws = TestWorkspace::answering([("/p/.keep", "")], |_| -> ExecResult {
        panic!("a host's workspace fails")
    });
    let job = hub
        .start(NewJob::new("make"), &ToolContext::new("/p", ws))
        .unwrap();
    let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv()).await;

    let ended = hub.get(&job.id).unwrap();
    assert_eq!(ended.status, JobStatus::Exited, "{ended:?}");
    assert!(ended.exit_code.is_some(), "{ended:?}");
    assert!(hub.running().is_empty());
    assert!(
        notice.is_ok_and(|n| n.is_some_and(|n| n.contains(&job.id))),
        "the model is told"
    );
}
