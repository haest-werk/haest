//! Ported from core/background-jobs.test.ts, over the real workspace (real
//! processes, a real process group), plus what the Rust hub adds: any workspace
//! runs a job, and dropping the hub kills what runs.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use kern_core::workspace::Workspace;
use kern_workspace::RealWorkspace;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::{Job, JobEvents, JobHub, JobStatus, MAX_FINISHED, MAX_JOBS};
use crate::testkit::{TestWorkspace, stdout};

#[derive(Default)]
pub(crate) struct Recorder {
    pub exits: Mutex<Vec<Job>>,
    pub snapshots: Mutex<Vec<Vec<String>>>,
    pub chunks: Mutex<Vec<(String, String, bool)>>,
}

impl JobEvents for Recorder {
    fn on_change(&self, running: &[Job]) {
        let ids = running.iter().map(|j| j.id.clone()).collect();
        self.snapshots.lock().unwrap().push(ids);
    }
    fn on_output(&self, job: &Job, chunk: &str, done: bool) {
        let row = (job.id.clone(), chunk.to_string(), done);
        self.chunks.lock().unwrap().push(row);
    }
    fn on_exit(&self, job: &Job) {
        self.exits.lock().unwrap().push(job.clone());
    }
}

impl Recorder {
    pub fn exit_count(&self) -> usize {
        self.exits.lock().unwrap().len()
    }
}

pub(crate) struct Rig {
    pub hub: Arc<JobHub>,
    pub events: Arc<Recorder>,
    pub notices: UnboundedReceiver<String>,
    pub ws: Arc<dyn Workspace>,
    pub dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Rig {
    pub fn real() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (tx, notices) = unbounded_channel();
        let events = Arc::new(Recorder::default());
        Self {
            hub: Arc::new(JobHub::with_events(Arc::new(tx), events.clone())),
            events,
            notices,
            ws: Arc::new(RealWorkspace::new()),
            dir,
            _tmp: tmp,
        }
    }

    pub fn start(&self, command: &str) -> Job {
        self.hub
            .start(self.ws.clone(), command, command, self.dir.clone())
            .unwrap()
    }
}

/// Poll until `cond` holds or `ms` pass.
pub(crate) async fn until(ms: u64, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_millis(ms);
    while !cond() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The pid a job wrote to `child.pid` (a process it left in the background).
async fn child_pid(dir: &Path) -> libc::pid_t {
    let file = dir.join("child.pid");
    until(3000, || {
        std::fs::read_to_string(&file).is_ok_and(|s| s.ends_with('\n'))
    })
    .await;
    std::fs::read_to_string(file)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

async fn gone(pid: libc::pid_t) -> bool {
    until(3000, || !alive(pid)).await;
    !alive(pid)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_runs_on_its_own_and_its_exit_is_reported_once_with_the_code() {
    let mut r = Rig::real();
    let job = r.start("echo one; sleep 0.05; echo two; exit 3");
    until(3000, || r.events.exit_count() > 0).await;
    let exits: Vec<_> = r.events.exits.lock().unwrap().clone();
    let exits: Vec<_> = exits
        .iter()
        .map(|j| (j.id.clone(), j.status, j.exit_code))
        .collect();
    assert_eq!(exits, [(job.id.clone(), JobStatus::Exited, Some(3))]);
    assert_eq!(
        *r.events.snapshots.lock().unwrap(),
        [vec![job.id.clone()], vec![]]
    );
    let last = r.events.chunks.lock().unwrap().last().cloned();
    assert_eq!(last, Some((job.id.clone(), String::new(), true)));
    assert_eq!(r.hub.buffer(&job.id).unwrap(), "one\ntwo\n");
    let notice = r.notices.try_recv().expect("the exit reaches the inbox");
    assert!(notice.contains(&job.id));
    assert!(r.notices.try_recv().is_err(), "exactly one notice");
}

#[tokio::test(flavor = "multi_thread")]
async fn take_hands_out_only_what_is_new_since_the_last_read() {
    let r = Rig::real();
    let job = r.start("echo first; sleep 0.3; echo second");
    until(3000, || r.hub.buffer(&job.id).unwrap().contains("first")).await;
    assert_eq!(r.hub.take(&job.id).unwrap().fresh, "first\n");
    until(3000, || r.events.exit_count() > 0).await;
    assert_eq!(r.hub.take(&job.id).unwrap().fresh, "second\n");
    assert_eq!(r.hub.take(&job.id).unwrap().fresh, "");
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_stops_the_whole_process_group_and_is_reported_as_killed_not_exited() {
    let mut r = Rig::real();
    let job = r.start("sleep 30 & echo $! > child.pid; sleep 30; wait");
    let pid = child_pid(&r.dir).await;
    assert!(r.hub.kill(&job.id));
    assert!(!r.hub.kill(&job.id), "a job is killed once");
    until(3000, || r.events.exit_count() > 0).await;
    assert_eq!(r.events.exits.lock().unwrap()[0].status, JobStatus::Killed);
    assert!(r.hub.running().is_empty());
    assert!(gone(pid).await, "the job's own child outlived the kill");
    assert!(r.notices.try_recv().is_err(), "a killed job wakes nobody");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_last_finished_jobs_and_their_output_are_kept() {
    let r = Rig::real();
    let mut ids = Vec::new();
    for i in 0..=MAX_FINISHED {
        ids.push(r.start("echo done").id);
        until(3000, || r.events.exit_count() > i).await;
    }
    assert!(r.hub.get(&ids[0]).is_none());
    assert_eq!(r.hub.buffer(&ids[1]).unwrap(), "done\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_max_jobs_run_at_once() {
    let r = Rig::real();
    for _ in 0..MAX_JOBS {
        r.start("sleep 30");
    }
    let refused = r
        .hub
        .start(r.ws.clone(), "sleep 30", "one too many", r.dir.clone());
    assert!(refused.is_err());
    r.hub.kill_all();
    until(5000, || r.events.exit_count() == MAX_JOBS).await;
    assert_eq!(r.events.exit_count(), MAX_JOBS);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_cannot_stream_still_runs_a_job_and_hands_its_output_over_at_the_end() {
    let (tx, mut notices) = unbounded_channel();
    let hub = JobHub::new(Arc::new(tx));
    let ws = TestWorkspace::answering([("/p/.keep", "")], |_| stdout("built\n"));
    let job = hub.start(ws.clone(), "make", "build", "/p".into()).unwrap();
    let notice = tokio::time::timeout(Duration::from_secs(3), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(notice.contains(&job.id));
    assert_eq!(hub.take(&job.id).unwrap().fresh, "built\n");
    assert_eq!(hub.get(&job.id).unwrap().exit_code, Some(0));
    assert!(ws.calls()[0].shell);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_hub_kills_every_job_still_running() {
    let r = Rig::real();
    r.start("sleep 30 & echo $! > child.pid; sleep 30; wait");
    let pid = child_pid(&r.dir).await;
    drop(r);
    assert!(gone(pid).await, "a job outlived its hub");
}
