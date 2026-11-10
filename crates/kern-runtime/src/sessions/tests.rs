//! A listing reads each file's head only, growing it for a long first
//! question; a resume reads headers newest first and stops at its match.

use std::{
    io,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;

use super::*;
use crate::kern::{
    fs::{DirEntry, FsStat, MemFs},
    workspace::RealFs,
};

/// A MemFs that remembers every head read and refuses a whole-file read.
#[derive(Default)]
struct Spy {
    inner: MemFs,
    heads: Mutex<Vec<(String, usize)>>,
}

impl Spy {
    async fn with_sessions(sessions: &[(&str, &str, &str)]) -> Self {
        let spy = Self::default();
        spy.inner.mkdir(Path::new(DIR), true).await.unwrap();
        for (id, cwd, text) in sessions {
            let path = file_of(Path::new(DIR), id);
            let text = session_text(id, cwd, text);
            spy.inner.write(&path, text.as_bytes()).await.unwrap();
            // Distinct last writes, a millisecond apart at least.
            std::thread::sleep(Duration::from_millis(5));
        }
        spy
    }

    fn heads(&self) -> Vec<(String, usize)> {
        self.heads.lock().unwrap().clone()
    }
}

#[async_trait]
impl Fs for Spy {
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        panic!("{} read whole", p.display())
    }
    async fn read_head(&self, p: &Path, max: usize) -> io::Result<Vec<u8>> {
        let id = p.file_stem().unwrap().to_string_lossy().into_owned();
        self.heads.lock().unwrap().push((id, max));
        self.inner.read_head(p, max).await
    }
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.inner.write(p, d).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.inner.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.inner.stat(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.inner.read_dir(p).await
    }
    async fn mkdir(&self, p: &Path, r: bool) -> io::Result<()> {
        self.inner.mkdir(p, r).await
    }
    async fn remove(&self, p: &Path, r: bool) -> io::Result<()> {
        self.inner.remove(p, r).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.rename(a, b).await
    }
}

const DIR: &str = "/s";

/// A header, a torn line, then the first question.
fn session_text(id: &str, cwd: &str, ask: &str) -> String {
    let lines = [
        serde_json::to_string(&SessionRecord::header(id, id, cwd, 0)).unwrap(),
        "{torn".to_string(),
        serde_json::to_string(&SessionRecord::Message {
            t: 1,
            message: Message::user(ask),
        })
        .unwrap(),
    ];
    lines.join("\n")
}

fn tempdir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("kern-runtime-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_session(dir: &Path, id: &str, cwd: &str, ask: &str) {
    std::fs::write(file_of(dir, id), session_text(id, cwd, ask)).unwrap();
}

#[tokio::test]
async fn sessions_list_newest_first_with_their_cwd_and_first_question() {
    let dir = tempdir();
    write_session(&dir, "old", "/a", "first question\nmore");
    std::thread::sleep(Duration::from_millis(20));
    write_session(&dir, "new", "/b", "second");
    std::fs::write(dir.join("notes.txt"), "not a session").unwrap();
    let list = list_sessions(&RealFs, &dir).await;
    let ids: Vec<_> = list.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["new", "old"]);
    assert_eq!(list[1].preview, "first question");
    assert_eq!(list[1].cwd, "/a");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn resume_without_an_id_takes_the_latest_session_in_this_directory() {
    let dir = tempdir();
    write_session(&dir, "here-old", "/p", "x");
    std::thread::sleep(Duration::from_millis(20));
    write_session(&dir, "here-new", "/p", "y");
    std::thread::sleep(Duration::from_millis(20));
    write_session(&dir, "elsewhere", "/q", "z");
    let latest = |cwd: &'static str| latest_in(&RealFs, &dir, Path::new(cwd));
    assert_eq!(latest("/p").await.as_deref(), Some("here-new"));
    assert_eq!(latest("/none").await, None);
    assert!(
        list_sessions(&RealFs, &dir.join("missing"))
            .await
            .is_empty()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_long_first_question_grows_the_read_and_the_rest_is_never_read() {
    let ask = format!("the question\n{}", "x".repeat(FIRST_READ * 2));
    let spy = Spy::with_sessions(&[("long", "/p", &ask)]).await;
    let path = file_of(Path::new(DIR), "long");
    let rest = format!("\n{}", "y".repeat(MAX_READ * 4));
    spy.inner.append(&path, rest.as_bytes()).await.unwrap();
    let list = list_sessions(&spy, Path::new(DIR)).await;
    assert_eq!(
        (list[0].cwd.as_str(), list[0].preview.as_str()),
        ("/p", "the question")
    );
    let reads: Vec<usize> = spy.heads().into_iter().map(|(_, n)| n).collect();
    assert_eq!(reads[0], FIRST_READ);
    assert!(reads.iter().all(|&n| n < MAX_READ), "{reads:?}");
}

#[tokio::test]
async fn a_resume_reads_one_head_per_session_newest_first_and_stops_at_its_match() {
    let spy = Spy::with_sessions(&[
        ("old-p", "/p", "a"),
        ("new-p", "/p", "b"),
        ("newest-q", "/q", "c"),
    ])
    .await;
    let latest = latest_in(&spy, Path::new(DIR), Path::new("/p")).await;
    assert_eq!(latest.as_deref(), Some("new-p"));
    let read = spy.heads();
    let ids: Vec<&str> = read.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["newest-q", "new-p"]);
    assert!(read.iter().all(|&(_, n)| n == FIRST_READ));
}
