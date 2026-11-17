//! A panic while a job runs, in a host's [`JobEvents::on_output`] or in the
//! workspace: the job is still reported ended.

use std::{path::Path, sync::Arc, time::Duration};

use haest_core::{
    fs::MemFs,
    tool::ToolContext,
    workspace::{ExecResult, WorkspaceRef},
};
use haest_workspace::{RealWorkspace, SandboxWorkspace};
use tokio::sync::mpsc::unbounded_channel;

use super::Recorder;
use crate::{
    jobs::{Job, JobEvents, JobHub, JobOutput, JobStatus, NewJob},
    testkit::TestWorkspace,
};

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
    let sandbox: WorkspaceRef = SandboxWorkspace::over(MemFs::new()).into();
    let real: WorkspaceRef = RealWorkspace::new(std::env::vars().collect()).into();
    for (name, ws, cwd) in [
        ("sandbox", sandbox, Path::new("/")),
        ("real", real, tmp.path()),
    ] {
        let (inbox, mut notices) = unbounded_channel();
        let hub = JobHub::new(inbox);
        let events = Arc::new(PanicsOnOutput::default());
        hub.set_events(events.clone());
        let command = NewJob::new("echo one; sleep 0.2; echo two; exit 3");
        let job = hub.start(command, &ToolContext::new(cwd, ws)).unwrap();
        // The notice is the last thing a job's end does.
        let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv()).await;

        let ended = hub.get(&job.id).unwrap();
        assert_eq!(
            (ended.status, ended.exit_code),
            (JobStatus::Exited, Some(3)),
            "{name}"
        );
        assert!(hub.running().is_empty(), "{name}");
        assert_eq!(hub.buffer(&job.id).unwrap(), "one\ntwo\n", "{name}");
        let done = events.0.chunks.lock().unwrap().iter().any(|row| row.2);
        assert!(done, "{name}: Done is heard");
        assert_eq!(events.0.exit_count(), 1, "{name}");
        assert!(
            notice.is_ok_and(|n| n.is_some_and(|n| n.contains(&job.id))),
            "{name}: the model is told"
        );
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
