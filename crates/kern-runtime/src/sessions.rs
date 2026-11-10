//! Saved sessions, through the host's [`Fs`]: the list, and which file a
//! resume without an id means. Only the head of each file is read (the header
//! and the first user message), so listing stays fast however long the
//! conversations got.

use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use crate::kern::{Message, fs::Fs, session::SessionRecord};

/// Enough of a session file for its header and first question.
const HEAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    /// Last write, ms since the epoch.
    pub modified_ms: u64,
    /// The first line of the first thing the human asked.
    pub preview: String,
}

/// A new session id: a v4 UUID, so ids never collide across machines.
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn head_of(fs: &dyn Fs, path: &Path) -> Option<(String, String)> {
    let bytes = fs.read_head(path, HEAD_BYTES).await.ok()?;
    let mut cwd = None;
    for line in String::from_utf8_lossy(&bytes).lines().take(200) {
        let Ok(rec) = serde_json::from_str::<SessionRecord>(line) else {
            continue;
        };
        match rec {
            SessionRecord::Header { cwd: c, .. } => cwd = Some(c),
            SessionRecord::Message {
                message: Message::User(u),
                ..
            } => {
                let first = u.content.lines().next().unwrap_or("").chars().take(70);
                return Some((cwd?, first.collect()));
            }
            _ => {}
        }
    }
    cwd.map(|c| (c, String::new()))
}

async fn info(fs: &dyn Fs, dir: &Path, name: &str) -> Option<SessionInfo> {
    let id = name.strip_suffix(".jsonl")?.to_string();
    let path = dir.join(name);
    let modified = fs.stat(&path).await.ok()?.modified?;
    let modified_ms = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    let (cwd, preview) = head_of(fs, &path).await?;
    Some(SessionInfo {
        id,
        cwd,
        modified_ms,
        preview,
    })
}

/// Every session in `dir`, newest first. Unreadable files are skipped.
pub async fn list_sessions(fs: &dyn Fs, dir: &Path) -> Vec<SessionInfo> {
    let Ok(entries) = fs.read_dir(dir).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries {
        out.extend(info(fs, dir, &e.name).await);
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified_ms));
    out
}

/// The most recently written session started in `cwd`.
pub async fn latest_in(fs: &dyn Fs, dir: &Path, cwd: &Path) -> Option<String> {
    let here = cwd.to_string_lossy();
    list_sessions(fs, dir)
        .await
        .into_iter()
        .find(|s| s.cwd == here)
        .map(|s| s.id)
}

pub fn sessions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::kern::workspace::RealFs;

    fn tempdir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("kern-runtime-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_session(dir: &Path, id: &str, cwd: &str, ask: &str) {
        let lines = [
            serde_json::to_string(&SessionRecord::header(id, id, cwd, 0)).unwrap(),
            "{torn".to_string(),
            serde_json::to_string(&SessionRecord::Message {
                t: 1,
                message: Message::user(ask),
            })
            .unwrap(),
        ];
        std::fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n")).unwrap();
    }

    #[tokio::test]
    async fn sessions_list_newest_first_with_their_cwd_and_first_question() {
        let dir = tempdir();
        write_session(&dir, "old", "/a", "first question\nmore");
        std::thread::sleep(std::time::Duration::from_millis(20));
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
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(&dir, "here-new", "/p", "y");
        std::thread::sleep(std::time::Duration::from_millis(20));
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
    async fn only_the_head_of_a_long_session_is_read() {
        let dir = tempdir();
        write_session(&dir, "long", "/p", "the question");
        let tail = "x".repeat(HEAD_BYTES * 2);
        let path = dir.join("long.jsonl");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push('\n');
        text.push_str(&tail);
        std::fs::write(&path, text).unwrap();
        let head = RealFs.read_head(&path, HEAD_BYTES).await.unwrap();
        assert_eq!(head.len(), HEAD_BYTES);
        assert_eq!(
            list_sessions(&RealFs, &dir).await[0].preview,
            "the question"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
