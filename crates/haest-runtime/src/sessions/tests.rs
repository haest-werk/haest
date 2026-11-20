//! A listing reads each file's head only, growing it for a long first
//! question; a resume reads headers newest first and stops at its match.

use std::{
    io,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use haest_core::{
    fs::{DirEntry, FsStat, MemFs},
    session::SessionHeader,
};
use haest_workspace::RealFs;

use super::*;

/// A MemFs that remembers every head read and refuses a whole-file read.
#[derive(Default)]
struct Spy {
    inner: MemFs,
    heads: Mutex<Vec<(String, usize)>>,
    /// Every file last written at the same instant, and a stat answers the
    /// later the earlier its id sorts, as a busy disk might.
    tied: bool,
}

impl Spy {
    async fn with_sessions(sessions: &[(&str, &str, &str)]) -> Self {
        let spy = Self::default();
        spy.inner.create_dir_all(Path::new(DIR)).await.unwrap();
        for (id, cwd, text) in sessions {
            let path = session_file(Path::new(DIR), id).unwrap();
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
        let mut stat = self.inner.stat(p).await?;
        if self.tied {
            let first = p.file_stem().and_then(|s| s.to_str()?.bytes().next());
            for _ in 0..b'z'.saturating_sub(first.unwrap_or(b'z')) {
                tokio::task::yield_now().await;
            }
            stat.modified = Some(UNIX_EPOCH + Duration::from_secs(1));
        }
        Ok(stat)
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.inner.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir(p).await
    }
    async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        self.inner.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        self.inner.remove_all(p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.rename(a, b).await
    }
}

const DIR: &str = "/s";

/// A header, a torn line, then the first question.
fn session_text(id: &str, cwd: &str, ask: &str) -> String {
    let lines = [
        serde_json::to_string(&SessionRecord::from(SessionHeader {
            session_id: id.into(),
            conversation_id: id.into(),
            cwd: cwd.into(),
            created_ms: 0,
        }))
        .unwrap(),
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
    let d = std::env::temp_dir().join(format!("haest-runtime-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_session(dir: &Path, id: &str, cwd: &str, ask: &str) {
    let path = session_file(dir, id).unwrap();
    std::fs::write(path, session_text(id, cwd, ask)).unwrap();
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
    assert_eq!(list[1].cwd, Path::new("/a"));
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
async fn a_long_first_question_takes_one_more_read_and_the_rest_is_never_read() {
    let ask = format!("the question\n{}", "x".repeat(FIRST_READ * 2));
    let spy = Spy::with_sessions(&[("long", "/p", &ask)]).await;
    let path = session_file(Path::new(DIR), "long").unwrap();
    let rest = format!("\n{}", "y".repeat(MAX_READ * 4));
    spy.inner.append(&path, rest.as_bytes()).await.unwrap();
    let list = list_sessions(&spy, Path::new(DIR)).await;
    assert_eq!(
        (list[0].cwd.as_path(), list[0].preview.as_str()),
        (Path::new("/p"), "the question")
    );
    let reads: Vec<usize> = spy.heads().into_iter().map(|(_, n)| n).collect();
    assert_eq!(reads, [FIRST_READ, MAX_READ]);
}

#[tokio::test]
async fn a_file_exactly_one_read_long_is_read_once_and_whole() {
    let fill = FIRST_READ - session_text("exact", "/p", "").len();
    let ask = "q".repeat(fill);
    let spy = Spy::with_sessions(&[("exact", "/p", &ask)]).await;
    let list = list_sessions(&spy, Path::new(DIR)).await;
    assert_eq!(list[0].preview, ask[..70]);
    assert_eq!(spy.heads(), [("exact".to_string(), FIRST_READ)]);
}

#[tokio::test]
async fn sessions_last_written_at_the_same_instant_come_in_id_order() {
    let mut spy = Spy::with_sessions(&[("c", "/p", "3"), ("a", "/p", "1"), ("b", "/p", "2")]).await;
    spy.tied = true;
    let list = list_sessions(&spy, Path::new(DIR)).await;
    let ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c"]);
    let latest = latest_in(&spy, Path::new(DIR), Path::new("/p")).await;
    assert_eq!(latest.as_deref(), Some("a"));
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
