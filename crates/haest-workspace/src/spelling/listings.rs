//! The directory listings a lookup reads, kept while their directory stays
//! as it was.

use std::{
    collections::HashMap,
    ffi::OsString,
    fmt,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long ago a directory must have last changed for its listing to be
/// kept. A disk stamps a change at its own clock's resolution (a whole
/// second on HFS+, two on FAT, a few milliseconds on Linux's coarse clock),
/// so a change right after a read can carry the very stamp the read saw.
pub(super) const SETTLED: Duration = Duration::from_secs(2);

/// Listings kept at most; past that every one is forgotten and read again.
const KEPT: usize = 4096;

/// The names one directory lists, each with the inode number the listing
/// gives it.
pub(super) type Names = Arc<HashMap<OsString, u64>>;

/// Directory listings, by the directory's path as asked.
#[derive(Default)]
pub(super) struct Listings {
    kept: Mutex<HashMap<PathBuf, Listing>>,
    /// Listings read from the disk, for the tests.
    #[cfg(test)]
    pub(super) reads: std::sync::atomic::AtomicUsize,
}

/// One directory's names, and the stamp of the directory they were read
/// from.
struct Listing {
    stamp: Stamp,
    names: Names,
}

impl Listings {
    /// `dir`'s listing, if one is kept and `dir` is still as it was then.
    pub(super) fn kept(&self, dir: &Path, now: Stamp) -> Option<Names> {
        let kept = self.lock();
        let listing = kept.get(dir)?;
        (listing.stamp == now).then(|| listing.names.clone())
    }

    pub(super) fn keep(&self, dir: &Path, stamp: Stamp, names: Names) {
        let mut kept = self.lock();
        if kept.len() >= KEPT {
            kept.clear();
        }
        kept.insert(dir.to_path_buf(), Listing { stamp, names });
    }

    /// Every name `dir` lists, from the disk; None when it cannot be read.
    pub(super) async fn read(&self, dir: &Path) -> Option<Names> {
        #[cfg(test)]
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut entries = tokio::fs::read_dir(dir).await.ok()?;
        let mut names = HashMap::new();
        while let Some(e) = entries.next_entry().await.ok()? {
            names.insert(e.file_name(), e.ino());
        }
        Some(Arc::new(names))
    }

    /// Held only to look up or keep a listing, never across an await, so a
    /// poisoned one is a bug here.
    fn lock(&self) -> MutexGuard<'_, HashMap<PathBuf, Listing>> {
        self.kept
            .lock()
            .expect("the spelling's listings lock poisoned")
    }
}

/// How many listings are kept; never the names.
impl fmt::Debug for Listings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kept = self.kept.try_lock().map(|kept| kept.len());
        let mut out = f.debug_struct("Listings");
        match kept {
            Ok(kept) => out.field("kept", &kept),
            Err(_) => out.field("kept", &format_args!("<locked>")),
        };
        out.finish_non_exhaustive()
    }
}

/// Which directory a path led to, and when it last changed: adding,
/// removing or renaming an entry moves a directory's ctime, which no call
/// can set back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    dev: u64,
    ino: u64,
    changed: (i64, i64),
}

impl Stamp {
    pub(super) fn of(m: &std::fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }

    /// Whether the directory last changed at least `settled` before `now`.
    pub(super) fn settled(&self, now: SystemTime, settled: Duration) -> bool {
        let (Ok(secs), Ok(nanos)) = (u64::try_from(self.changed.0), u32::try_from(self.changed.1))
        else {
            return false;
        };
        let changed = UNIX_EPOCH.checked_add(Duration::new(secs, nanos));
        changed.is_some_and(|c| now.duration_since(c).is_ok_and(|age| age >= settled))
    }
}
