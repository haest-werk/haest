//! On the real disk: a second run of a session is refused while the first
//! runs, also where its lock file is another user's or its sessions dir
//! takes no new file, and a /clear moves the hold to the new session.

use std::{
    fs::Permissions,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use haest_core::testing::ScriptedProvider;
use haest_settings::Settings;

use crate::{
    BuildError, BuildOptions, Resume, Runtime, RuntimeInputs, SessionFileError, World, build,
    haest::{ChildEnv, WorkspaceMode},
    testkit::{Deny, Recorded},
};

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

/// A file or dir at a mode the test set, given `after` back as this drops,
/// even when the test fails: a dir left 0500 could not be removed.
struct Mode(PathBuf, u32);

impl Mode {
    fn set(path: &Path, mode: u32, after: u32) -> Self {
        std::fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
        Self(path.to_path_buf(), after)
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, Permissions::from_mode(self.1));
    }
}

/// A saved session whose lock file this user cannot open (one another user
/// left behind, a run under sudo say), or whose sessions dir takes no new
/// file, still keeps a second run out: the disk locks, so the run holds
/// the session through its own file, and no one is warned.
#[tokio::test]
async fn a_lock_file_this_user_cannot_open_still_keeps_a_second_run_out() {
    for case in ["foreign-lock", "read-only-dir"] {
        let dir = Scratch::new(case);
        let first = dir.build(Resume::New).await.unwrap();
        let id = first.session_id();
        first.shut_down().await;
        drop(first);
        let sessions = dir.0.join("data/sessions");
        let lock = sessions.join(format!("{id}.lock"));
        let _taken = if case == "foreign-lock" {
            Mode::set(&lock, 0o000, 0o600)
        } else {
            std::fs::remove_file(&lock).unwrap();
            Mode::set(&sessions, 0o500, 0o700)
        };

        let one = dir.resume_once_free(&id).await;
        let two = dir.build(Resume::Id(id.clone())).await;
        let warned = one.as_ref().ok().map(|rt| rt.ui().warnings().len());
        let failed = one.as_ref().err();
        assert_eq!(warned, Some(0), "{case}: held, no warning: {failed:?}");
        assert!(in_use(&two, &id), "{case}: {two:?}");
    }
}

/// A new session in a sessions dir its owner made read-only (0500) is held
/// all the same, with no warning: the dir is made private (0700) before the
/// lock file is made in it, as the session's file needs it anyway. A second
/// run of the session is refused while the first runs.
#[tokio::test]
async fn a_new_session_in_a_read_only_sessions_dir_keeps_a_second_run_out() {
    let dir = Scratch::new("new-in-read-only-dir");
    let sessions = dir.0.join("data/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let _taken = Mode::set(&sessions, 0o500, 0o700);

    let first = dir.build(Resume::New).await.unwrap();
    let id = first.session_id();
    let second = dir.build(Resume::Id(id.clone())).await;
    assert_eq!(first.ui().warnings().len(), 0, "held, no warning");
    assert!(in_use(&second, &id), "{second:?}");
}
