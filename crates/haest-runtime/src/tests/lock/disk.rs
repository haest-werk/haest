//! On the real disk: a second run of a session is refused while the first
//! runs, and a /clear moves the hold to the new session.

use std::{
    path::PathBuf,
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
