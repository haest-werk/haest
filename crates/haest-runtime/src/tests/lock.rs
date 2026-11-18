//! One run per session: a run holds its session until it is dropped, a
//! second run of it is refused, a new session is held before its file is
//! written, and a file system that cannot lock runs unlocked, with a warning.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use haest_core::testing::{ScriptedProvider, text_answer};
use haest_settings::Settings;

use super::{hardcoded, lifecycle::said};
use crate::{
    BuildError, BuildOptions, Resume, Runtime, RuntimeInputs, SessionFileError, World, build,
    haest::{
        ChildEnv, LoopEvent, WorkspaceMode,
        core::fs::{DirEntry, Fs, FsLock, FsStat, MemFs},
        core::session::session_id_of,
    },
    testkit::{Deny, Recorded},
};

const SESSIONS: &str = "/data/sessions";

/// A host's file system in memory with locks that fail: with `refusal()`
/// once one is set, else it holds nothing (no locks).
#[derive(Default)]
struct Locks {
    files: MemFs,
    refusal: Mutex<Option<fn() -> io::Error>>,
}

impl Locks {
    fn refusing(refusal: fn() -> io::Error) -> Arc<Self> {
        let fs = Arc::new(Self::default());
        fs.refuse(refusal);
        fs
    }

    fn refuse(&self, refusal: fn() -> io::Error) {
        *self.refusal.lock().unwrap() = Some(refusal);
    }

    /// The names of the session files in the sessions dir.
    async fn sessions(&self) -> Vec<String> {
        let entries = self.files.read_dir(Path::new(SESSIONS)).await;
        let names = entries.unwrap_or_default().into_iter().map(|e| e.name);
        names.filter(|n| session_id_of(n).is_some()).collect()
    }
}

#[async_trait]
impl Fs for Locks {
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.files.read(p).await
    }
    async fn read_head(&self, p: &Path, max: usize) -> io::Result<Vec<u8>> {
        self.files.read_head(p, max).await
    }
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.files.write(p, d).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.files.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.files.stat(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.files.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.files.create_dir(p).await
    }
    async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.files.create_dir_all(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        self.files.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        self.files.remove_all(p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.files.rename(a, b).await
    }
    async fn try_lock(&self, _: &Path) -> io::Result<Option<FsLock>> {
        match *self.refusal.lock().unwrap() {
            Some(refusal) => Err(refusal()),
            None => Ok(None),
        }
    }
}

/// A conversation over `fs` whose model answers `answer` once, with no key
/// and no network; `resume` as [`BuildOptions`].
async fn over(
    fs: &Arc<Locks>,
    resume: Resume,
    answer: &str,
) -> Result<Runtime<Recorded>, BuildError> {
    let model = ScriptedProvider::new([text_answer(answer)]);
    let inputs =
        RuntimeInputs::new(hardcoded(), World::files(fs.clone())).with_models(model.resolver());
    let opts = BuildOptions {
        resume,
        ..BuildOptions::default()
    };
    build(inputs, Recorded::default(), Deny, opts).await
}

/// A run of `resume` over `fs` that asks `question` and shuts down: the
/// session's id, and how many lines the Ui was warned or told of a failure.
async fn one_turn(fs: &Arc<Locks>, resume: Resume, question: &str) -> (String, usize) {
    let mut rt = over(fs, resume, "an answer").await.unwrap();
    let outcome = rt.human_turn(question, &mut |_: LoopEvent| {}).await;
    assert_eq!(outcome.error, None, "{outcome:?}");
    rt.shut_down().await;
    (rt.session_id(), rt.ui().warnings().len())
}

/// A lock the file system cannot take (an I/O error, not another holder)
/// costs nothing of the session: a new run and a resume both go on
/// unlocked, as before sessions were locked, and every turn lands and
/// replays. The human is warned, once a run, that another run could write
/// the session too.
#[tokio::test]
async fn a_lock_the_file_system_cannot_take_runs_unlocked_with_a_warning() {
    let fs = Locks::refusing(|| io::Error::from_raw_os_error(libc::EIO));
    let (id, warned) = one_turn(&fs, Resume::New, "first question").await;
    assert_eq!(warned, 1, "a new run: warned once, and nothing failed");
    let resume = Resume::Id(id.clone());
    let (_, warned) = one_turn(&fs, resume, "second question").await;
    assert_eq!(warned, 1, "a resume: warned once, and nothing failed");

    let back = over(&fs, Resume::Id(id), "unused").await.unwrap();
    let history = back.agent().history();
    assert_eq!(said(history), ["first question", "second question"]);
}

/// A new session whose lock another holder has fails as in use before its
/// file is written, so nothing is left for a resume to pick up; a /clear
/// into one changes nothing.
#[tokio::test]
async fn a_new_session_in_use_fails_before_its_file_is_written() {
    let fs = Arc::new(Locks::default());
    let mut rt = over(&fs, Resume::New, "unused").await.unwrap();
    rt.hooks().flush().await;
    let id = rt.session_id();
    fs.refuse(|| io::ErrorKind::WouldBlock.into());

    let cleared = rt.clear().await;
    let in_use = matches!(cleared, Err(SessionFileError::InUse { .. }));
    assert!(in_use, "{cleared:?}");
    assert_eq!(rt.session_id(), id);
    let built = over(&fs, Resume::New, "unused").await;
    let in_use = matches!(
        built,
        Err(BuildError::SessionFile(SessionFileError::InUse { .. }))
    );
    assert!(in_use, "{built:?}");
    assert_eq!(fs.sessions().await, [format!("{id}.jsonl")]);
}

/// A dir of the test's own under the system's temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("haest-{name}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("w")).unwrap();
        Self(dir)
    }

    /// A conversation on the real disk in this dir; `resume` as [`BuildOptions`].
    async fn build(&self, resume: Resume) -> Result<Runtime<Recorded>, BuildError> {
        let settings = Settings {
            workspace: WorkspaceMode::Shut,
            data_dir: self.0.join("data"),
            ..Settings::bare(self.0.join("w"), self.0.clone())
        };
        let model = ScriptedProvider::new([]);
        let inputs = RuntimeInputs::new(settings, World::disk(ChildEnv::default()))
            .with_models(model.resolver());
        let opts = BuildOptions {
            resume,
            ..BuildOptions::default()
        };
        build(inputs, Recorded::default(), Deny, opts).await
    }

    /// [`build`](Self::build) a resume of `id` once no run holds it, waiting
    /// at most 5 s. A process another test forks shares this one's open
    /// files until it starts its program, so a lock just dropped can stay
    /// held that long.
    async fn resume_once_free(&self, id: &str) -> Result<Runtime<Recorded>, BuildError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let built = self.build(Resume::Id(id.to_string())).await;
            if !in_use(&built, id) || Instant::now() > deadline {
                return built;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `built` failed because another run holds the session `id`.
fn in_use(built: &Result<Runtime<Recorded>, BuildError>, id: &str) -> bool {
    let Err(BuildError::SessionFile(SessionFileError::InUse { id: held })) = built else {
        return false;
    };
    held == id
}

/// A session one run writes is not resumed by another while it runs (`haest
/// -r` in a second terminal): that build fails, saying the session is in use.
/// Once the first run is gone, the session resumes.
#[tokio::test]
async fn a_session_another_run_writes_is_refused_until_that_run_is_gone() {
    let dir = Scratch::new("held-session");
    let first = dir.build(Resume::New).await.unwrap();
    let id = first.session_id();

    for resume in [Resume::Id(id.clone()), Resume::Latest] {
        let second = dir.build(resume).await;
        assert!(in_use(&second, &id), "{second:?}");
    }
    first.shut_down().await;
    drop(first);
    let again = dir.resume_once_free(&id).await;
    assert!(again.is_ok(), "{again:?}");
    let held = dir.build(Resume::Id(id.clone())).await;
    assert!(in_use(&held, &id), "the resumed run holds it now: {held:?}");
}

/// A /clear holds the new session and lets the old one go: another run
/// resumes the old one, not the new.
#[tokio::test]
async fn a_clear_holds_the_new_session_and_lets_the_old_one_go() {
    let dir = Scratch::new("clear-held");
    let mut first = dir.build(Resume::New).await.unwrap();
    let old = first.session_id();
    first.clear().await.unwrap();
    let new = first.session_id();

    let on_new = dir.build(Resume::Id(new.clone())).await;
    assert!(in_use(&on_new, &new), "{on_new:?}");
    let on_old = dir.resume_once_free(&old).await;
    assert!(on_old.is_ok(), "{on_old:?}");
}
