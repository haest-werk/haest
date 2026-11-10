//! Saved sessions, through the host's [`Fs`]: the list, and which file a
//! resume without an id means. Only the head of each file is read: the header
//! and the first user message, in [`FIRST_READ`] bytes unless the first
//! question is longer, so listing stays fast however long the conversations
//! got. The paths and which file names are sessions are kern-core's
//! ([`session_file`], [`session_id_of`]).

use std::{
    future::ready,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use futures::{StreamExt, stream};

use crate::kern::{
    Message,
    fs::Fs,
    session::{SessionRecord, session_file, session_id_of},
};

/// The first read of a session file: the header and, nearly always, the first
/// question. When the question is not in it, one more read of [`MAX_READ`].
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

/// A session file in the dir: its id, where it is, its last write and size.
struct Entry {
    id: String,
    path: PathBuf,
    modified_ms: u64,
    size: u64,
}

impl Entry {
    /// Its first `max` bytes, and whether they are the whole file.
    async fn head(&self, fs: &dyn Fs, max: usize) -> Option<(Vec<u8>, bool)> {
        let bytes = fs.read_head(&self.path, max).await.ok()?;
        let whole = bytes.len() as u64 >= self.size;
        Some((bytes, whole))
    }
}

/// The cwd and first question, reading only as far as the question: the
/// first read nearly always holds it, a longer one takes one more read.
async fn head_of(fs: &dyn Fs, e: &Entry) -> Option<(String, String)> {
    for max in [FIRST_READ, MAX_READ] {
        let (bytes, whole) = e.head(fs, max).await?;
        match scan(&bytes, whole) {
            (cwd, Some(preview)) => return Some((cwd?, preview)),
            (cwd, None) if whole || max == MAX_READ => return cwd.map(|c| (c, String::new())),
            _ => {}
        }
    }
    None
}

/// The cwd a session started in: its header, in the first read.
async fn cwd_of(fs: &dyn Fs, e: &Entry) -> Option<String> {
    let (bytes, whole) = e.head(fs, FIRST_READ).await?;
    scan(&bytes, whole).0
}

async fn entry(fs: &dyn Fs, dir: &Path, id: &str) -> Option<Entry> {
    let path = session_file(dir, id).ok()?;
    let stat = fs.stat(&path).await.ok()?;
    let modified = stat.modified?.duration_since(UNIX_EPOCH).ok()?;
    Some(Entry {
        id: id.to_string(),
        path,
        modified_ms: modified.as_millis() as u64,
        size: stat.size,
    })
}

/// The sessions in `dir`, newest first. Files last written at the same
/// instant come in id order, so the order is the same on every call.
async fn newest_first(fs: &dyn Fs, dir: &Path) -> Vec<Entry> {
    let Ok(entries) = fs.read_dir(dir).await else {
        return Vec::new();
    };
    let ids = entries.iter().filter_map(|e| session_id_of(&e.name));
    let mut out: Vec<Entry> = stream::iter(ids)
        .map(|id| entry(fs, dir, id))
        .buffer_unordered(AT_ONCE)
        .filter_map(ready)
        .collect()
        .await;
    out.sort_by(|a, b| (b.modified_ms.cmp(&a.modified_ms)).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Every session in `dir`, newest first. Unreadable files are skipped.
pub async fn list_sessions(fs: &dyn Fs, dir: &Path) -> Vec<SessionInfo> {
    stream::iter(newest_first(fs, dir).await)
        .map(|e| async move {
            let (cwd, preview) = head_of(fs, &e).await?;
            Some(SessionInfo {
                id: e.id,
                cwd,
                modified_ms: e.modified_ms,
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
    for e in newest_first(fs, dir).await {
        if cwd_of(fs, &e).await.is_some_and(|c| c == here) {
            return Some(e.id);
        }
    }
    None
}

#[cfg(test)]
mod tests;
