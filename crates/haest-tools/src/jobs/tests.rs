//! Background jobs over the real workspace (real processes, a real process
//! group); any workspace runs a job, and dropping the hub kills what runs.

mod panics;
mod set_events;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use haest_core::{tool::ToolContext, workspace::ExecResult};
use haest_workspace::RealWorkspace;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::{
    Job, JobError, JobEvents, JobHub, JobOutput, JobStatus, MAX_FINISHED, MAX_JOBS, NewJob,
};
use crate::testkit::TestWorkspace;

#[derive(Default)]
pub(crate) struct Recorder {
    pub exits: Mutex<Vec<Job>>,
    pub snapshots: Mutex<Vec<Vec<String>>>,
    /// (job, chunk, done): a Done is an empty chunk with `true`.
    pub chunks: Mutex<Vec<(String, String, bool)>>,
}

impl JobEvents for Recorder {
    fn on_change(&self, running: &[Job]) {
        let ids = running.iter().map(|j| j.id.clone()).collect();
        self.snapshots.lock().unwrap().push(ids);
    }
    fn on_output(&self, job: &Job, output: JobOutput<'_>) {
        let row = match output {
            JobOutput::Chunk(chunk) => (job.id.clone(), chunk.to_string(), false),
            _ => (job.id.clone(), String::new(), true),
        };
        self.chunks.lock().unwrap().push(row);
    }
    fn on_exit(&self, job: &Job) {
        self.exits.lock().unwrap().push(job.clone());
    }
}

impl Recorder {
    pub(crate) fn exit_count(&self) -> usize {
        self.exits.lock().unwrap().len()
    }
}

pub(crate) struct Rig {
    pub hub: JobHub,
    pub events: Arc<Recorder>,
    pub notices: UnboundedReceiver<String>,
    /// The real workspace, in `dir`.
    pub ctx: ToolContext,
    pub dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Rig {
    pub(crate) fn real() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (tx, notices) = unbounded_channel();
        let events = Arc::new(Recorder::default());
        let ws = RealWorkspace::new(std::env::vars().collect());
        let hub = JobHub::new(tx);
        hub.set_events(events.clone());
        Self {
            hub,
            events,
            notices,
            ctx: ToolContext::new(&dir, ws),
            dir,
            _tmp: tmp,
        }
    }

    pub(crate) fn start(&self, command: &str) -> Job {
        self.hub.start(NewJob::new(command), &self.ctx).unwrap()
    }
}

/// Poll until `cond` holds or `ms` pass.
pub(crate) async fn until(ms: u64, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_millis(ms);
    while !cond() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[expect(
    unsafe_code,
    reason = "kill(2) with signal 0 asks whether a pid lives; std cannot"
)]
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
    let one_more = NewJob::new("sleep 30").with_description("one too many");
    let refused = r.hub.start(one_more, &r.ctx);
    assert!(matches!(refused, Err(JobError::TooMany { max: MAX_JOBS })));
    r.hub.kill_all();
    until(5000, || r.events.exit_count() == MAX_JOBS).await;
    assert_eq!(r.events.exit_count(), MAX_JOBS);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_cannot_stream_still_runs_a_job_and_hands_its_output_over_at_the_end() {
    let (tx, mut notices) = unbounded_channel();
    let hub = JobHub::new(tx);
    let ws = TestWorkspace::answering([("/p/.keep", "")], |_| ExecResult::ok("built\n"));
    let job = hub
        .start(
            NewJob::new("make").with_description("build"),
            &ToolContext::new("/p", ws.clone()),
        )
        .unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn dropping_one_clone_of_the_hub_kills_nothing_the_last_one_kills_all() {
    let r = Rig::real();
    let job = r.start("sleep 30 & echo $! > child.pid; sleep 30; wait");
    let pid = child_pid(&r.dir).await;
    let kept = r.hub.clone();
    drop(r);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(alive(pid), "dropping one clone killed the job");
    assert_eq!(
        kept.get(&job.id).map(|j| j.status),
        Some(JobStatus::Running)
    );
    drop(kept);
    assert!(gone(pid).await, "a job outlived the hub's last clone");
}

/// Hears the running sets, and holds the first one an end hands it (one job
/// left of two) until the test lets go, or drops `go`: a host that renders,
/// or takes a lock of its own, while the other job ends.
struct SlowRoster {
    /// The set held goes out here.
    entered: Mutex<Option<std::sync::mpsc::Sender<Vec<String>>>>,
    go: Mutex<std::sync::mpsc::Receiver<()>>,
    sets: Mutex<Vec<Vec<String>>>,
    exits: Mutex<usize>,
}

impl JobEvents for SlowRoster {
    fn on_change(&self, running: &[Job]) {
        let ids: Vec<String> = running.iter().map(|j| j.id.clone()).collect();
        let after_both_started = self.sets.lock().unwrap().iter().any(|s| s.len() == 2);
        if running.len() == 1 && after_both_started {
            let entered = self.entered.lock().unwrap().take();
            if let Some(entered) = entered {
                entered.send(ids.clone()).unwrap();
                let _ = self.go.lock().unwrap().recv();
            }
        }
        self.sets.lock().unwrap().push(ids);
    }
    fn on_exit(&self, _job: &Job) {
        *self.exits.lock().unwrap() += 1;
    }
}

/// Two jobs that end at once each handed their set over from their own task,
/// so the older set could arrive last: the host's roster then kept a job that
/// had ended. The last set the host hears is the hub's.
///
/// The test ends each job itself, so the ends overlap the same way every run.
/// Left to end at once, the two may never hand the host the set with one
/// left: it is folded into the newest when the other ends between the
/// first's change and its delivery (a host slow on the first's output, say).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_running_set_the_host_hears_is_the_hubs() {
    let r = Rig::real();
    let (entered, in_hook) = std::sync::mpsc::channel();
    let (go, held) = std::sync::mpsc::channel();
    let roster = Arc::new(SlowRoster {
        entered: Mutex::new(Some(entered)),
        go: Mutex::new(held),
        sets: Mutex::default(),
        exits: Mutex::default(),
    });
    r.hub.set_events(roster.clone());
    let first = r.start("sleep 30");
    let second = r.start("sleep 30");
    assert!(r.hub.kill(&first.id));
    // The first end's set is in the host's hands, held there...
    let set = tokio::task::spawn_blocking(move || in_hook.recv_timeout(Duration::from_secs(30)))
        .await
        .unwrap()
        .expect("an end handed its set over");
    assert_eq!(
        set,
        std::slice::from_ref(&second.id),
        "the set held is the second alone"
    );
    // ...while the other job ends, all the way to its exit.
    assert!(r.hub.kill(&second.id));
    until(30_000, || *roster.exits.lock().unwrap() == 1).await;
    let exits = *roster.exits.lock().unwrap();
    assert_eq!(exits, 1, "the second's exit, while the first's set is held");
    go.send(()).unwrap();
    until(30_000, || *roster.exits.lock().unwrap() == 2).await;
    assert!(r.hub.running().is_empty());
    let sets = roster.sets.lock().unwrap().clone();
    assert_eq!(
        sets.last(),
        Some(&vec![]),
        "the host kept an ended job: {sets:?}"
    );
}

/// Hears the running sets; one with a gate holds the first empty set until
/// the test lets go (a host that renders, or takes a lock of its own).
#[derive(Default)]
struct Gated {
    gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    sets: Mutex<Vec<Vec<String>>>,
}

impl JobEvents for Gated {
    fn on_change(&self, running: &[Job]) {
        if running.is_empty() {
            let gate = self.gate.lock().unwrap().take();
            if let Some((entered, go)) = gate {
                entered.send(()).unwrap();
                let _ = go.recv_timeout(Duration::from_secs(5));
            }
        }
        let ids = running.iter().map(|j| j.id.clone()).collect();
        self.sets.lock().unwrap().push(ids);
    }
}

/// The old listener was still being called when the host set a new one: the
/// running delivery handed every later set to the old listener, so the new
/// one heard nothing of a job started after the swap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listener_set_while_the_old_one_is_called_hears_every_set_from_then_on() {
    let r = Rig::real();
    let (entered, in_hook) = std::sync::mpsc::channel();
    let (go, held) = std::sync::mpsc::channel();
    let old = Arc::new(Gated {
        gate: Mutex::new(Some((entered, held))),
        ..Gated::default()
    });
    let new = Arc::new(Gated::default());
    r.hub.set_events(old.clone());
    r.start("true");
    // The job's end hands the old listener the empty set; it is slow with it.
    tokio::task::spawn_blocking(move || in_hook.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("the old listener heard the job end");
    r.hub.set_events(new.clone());
    let after = r.start("sleep 30");
    go.send(()).unwrap();
    let hubs = vec![after.id.clone()];
    until(3000, || new.sets.lock().unwrap().last() == Some(&hubs)).await;

    assert_eq!(
        new.sets.lock().unwrap().last(),
        Some(&hubs),
        "the new listener missed the start"
    );
    let old_sets = old.sets.lock().unwrap().clone();
    assert!(
        !old_sets.iter().any(|s| s.contains(&after.id)),
        "the replaced listener heard a start after the swap: {old_sets:?}"
    );
    r.hub.kill_all();
}

/// The model tells every state apart from what a job's state reads: an exit
/// carries its code, a running job how long it has run.
#[test]
fn every_job_state_reads_apart_with_its_code_or_its_time() {
    let started = Instant::now() - Duration::from_secs(42);
    let shown = |status, exit_code| {
        let job = Job {
            id: "b1".into(),
            command: "make".into(),
            description: "make".into(),
            status,
            exit_code,
            started,
        };
        job.state().to_string()
    };
    let states = [
        shown(JobStatus::Killed, None),
        shown(JobStatus::Exited, Some(3)),
        shown(JobStatus::Exited, Some(7)),
        shown(JobStatus::Exited, None),
        shown(JobStatus::Running, None),
    ];
    for (i, a) in states.iter().enumerate() {
        for b in &states[i + 1..] {
            assert_ne!(a, b, "two states read alike: {states:?}");
        }
    }
    assert!(
        states[1].contains('3') && states[2].contains('7'),
        "{states:?}"
    );
    let secs: String = states[4].chars().filter(char::is_ascii_digit).collect();
    let secs: u64 = secs.parse().expect("a running job says how long");
    assert!((42..100).contains(&secs), "{states:?}");
}
