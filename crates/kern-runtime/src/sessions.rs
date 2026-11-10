//! Saved sessions, through the host's [`Fs`]: the list, and which file a
//! resume without an id means. Only the head of each file is read: the header
//! and the first user message, in [`FIRST_READ`] bytes unless the first
//! question is longer, so listing stays fast however long the conversations
//! got.

use std::{
    cmp::Reverse,
    future::ready,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use futures::{StreamExt, stream};

use crate::kern::{Message, fs::Fs, session::SessionRecord};

/// The first read of a session file: the header and, nearly always, the first
/// question. Doubled while the first question is not in it yet.
const FIRST_READ: usize = 16 * 1024;
/// The most read of one file; a first question longer than this lists with no preview.
const MAX_READ: usize = 256 * 1024;
/// Files stat'd or read at once while listing.
const AT_ONCE: usize = 16;

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

pub fn sessions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions")
}

fn file_of(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jsonl"))
}

/// The header's cwd and the first question's first line, from the complete
/// lines of `bytes`. Unless `whole`, the last line may be cut off: not a
/// record yet.
fn scan(bytes: &[u8], whole: bool) -> (Option<String>, Option<String>) {
    let text = String::from_utf8_lossy(bytes);
    let complete = match text.rsplit_once('\n') {
        _ if whole => &text[..],
        Some((done, _)) => done,
        None => "",
    };
    let mut cwd = None;
    for line in complete.lines() {
        match serde_json::from_str::<SessionRecord>(line) {
            Ok(SessionRecord::Header { cwd: c, .. }) => cwd = Some(c),
            Ok(SessionRecord::Message {
                message: Message::User(u),
                ..
            }) => {
                let first = u.content.lines().next().unwrap_or("").chars().take(70);
                return (cwd, Some(first.collect()));
            }
            _ => {}
        }
    }
    (cwd, None)
}

/// The cwd and first question of the session at `path`, reading only as far
/// as the question.
async fn head_of(fs: &dyn Fs, path: &Path) -> Option<(String, String)> {
    let mut max = FIRST_READ;
    loop {
        let bytes = fs.read_head(path, max).await.ok()?;
        let whole = bytes.len() < max;
        match scan(&bytes, whole) {
            (cwd, Some(preview)) => return Some((cwd?, preview)),
            (cwd, None) if whole || max >= MAX_READ => return cwd.map(|c| (c, String::new())),
            _ => max *= 2,
        }
    }
}

/// The cwd a session started in: its header, in the first read.
async fn cwd_of(fs: &dyn Fs, path: &Path) -> Option<String> {
    let bytes = fs.read_head(path, FIRST_READ).await.ok()?;
    scan(&bytes, bytes.len() < FIRST_READ).0
}

async fn modified_ms(fs: &dyn Fs, dir: &Path, id: &str) -> Option<(String, u64)> {
    let modified = fs.stat(&file_of(dir, id)).await.ok()?.modified?;
    let ms = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    Some((id.to_string(), ms))
}

/// The session ids in `dir` (`<id>.jsonl`) with their last write, newest first.
async fn newest_first(fs: &dyn Fs, dir: &Path) -> Vec<(String, u64)> {
    let Ok(entries) = fs.read_dir(dir).await else {
        return Vec::new();
    };
    let ids = entries.iter().filter_map(|e| e.name.strip_suffix(".jsonl"));
    let mut out: Vec<_> = stream::iter(ids)
        .map(|id| modified_ms(fs, dir, id))
        .buffer_unordered(AT_ONCE)
        .filter_map(ready)
        .collect()
        .await;
    out.sort_by_key(|(_, ms)| Reverse(*ms));
    out
}

/// Every session in `dir`, newest first. Unreadable files are skipped.
pub async fn list_sessions(fs: &dyn Fs, dir: &Path) -> Vec<SessionInfo> {
    stream::iter(newest_first(fs, dir).await)
        .map(|(id, modified_ms)| async move {
            let (cwd, preview) = head_of(fs, &file_of(dir, &id)).await?;
            Some(SessionInfo {
                id,
                cwd,
                modified_ms,
                preview,
            })
        })
        .buffered(AT_ONCE)
        .filter_map(ready)
        .collect()
        .await
}

/// The most recently written session started in `cwd`: headers read newest
/// first, stopping at the first match.
pub async fn latest_in(fs: &dyn Fs, dir: &Path, cwd: &Path) -> Option<String> {
    let here = cwd.to_string_lossy();
    for (id, _) in newest_first(fs, dir).await {
        if cwd_of(fs, &file_of(dir, &id))
            .await
            .is_some_and(|c| c == here)
        {
            return Some(id);
        }
    }
    None
}

#[cfg(test)]
mod tests;
