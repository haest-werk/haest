//! Saved sessions on disk: the list, and which file a resume without an id
//! means. Only the head of each file is read (the header and the first user
//! message), so listing stays fast however long the conversations got.

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use crate::kern::{Message, session::SessionRecord};

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

fn head_of(path: &Path) -> Option<(String, String)> {
    let reader = BufReader::new(File::open(path).ok()?);
    let mut cwd = None;
    for line in reader.lines().take(200) {
        let Ok(rec) = serde_json::from_str::<SessionRecord>(&line.ok()?) else {
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

/// Every session in `dir`, newest first. Unreadable files are skipped.
pub fn list_sessions(dir: &Path) -> Vec<SessionInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<SessionInfo> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let id = path
                .file_name()?
                .to_str()?
                .strip_suffix(".jsonl")?
                .to_string();
            let modified = e.metadata().ok()?.modified().ok()?;
            let modified_ms = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
            let (cwd, preview) = head_of(&path)?;
            Some(SessionInfo {
                id,
                cwd,
                modified_ms,
                preview,
            })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.modified_ms));
    out
}

/// The most recently written session started in `cwd`.
pub fn latest_in(dir: &Path, cwd: &Path) -> Option<String> {
    let here = cwd.to_string_lossy();
    list_sessions(dir)
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

    #[test]
    fn sessions_list_newest_first_with_their_cwd_and_first_question() {
        let dir = tempdir();
        write_session(&dir, "old", "/a", "first question\nmore");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(&dir, "new", "/b", "second");
        std::fs::write(dir.join("notes.txt"), "not a session").unwrap();
        let list = list_sessions(&dir);
        let ids: Vec<_> = list.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["new", "old"]);
        assert_eq!(list[1].preview, "first question");
        assert_eq!(list[1].cwd, "/a");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resume_without_an_id_takes_the_latest_session_in_this_directory() {
        let dir = tempdir();
        write_session(&dir, "here-old", "/p", "x");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(&dir, "here-new", "/p", "y");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(&dir, "elsewhere", "/q", "z");
        assert_eq!(
            latest_in(&dir, Path::new("/p")).as_deref(),
            Some("here-new")
        );
        assert_eq!(latest_in(&dir, Path::new("/none")), None);
        assert!(list_sessions(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
