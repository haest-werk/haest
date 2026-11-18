//! One run per session: a run holds its session until it is dropped, a
//! second run of it is refused, a new session is held before its file is
//! written, and a file system that cannot lock runs unlocked, with a warning.
//! Here over a host's file system; `disk` on the real disk.

use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_core::testing::{ScriptedProvider, text_answer};

use super::{hardcoded, lifecycle::said};
use crate::{
    BuildError, BuildOptions, Resume, Runtime, RuntimeInputs, SessionFileError, World, build,
    haest::{
        LoopEvent,
        core::fs::{DirEntry, Fs, FsLock, FsStat, MemFs},
        core::session::session_id_of,
    },
    testkit::{Deny, Recorded},
};

mod disk;

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
/// replays. The human is warned, once for the session each run holds, that
/// another run could write it too.
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
