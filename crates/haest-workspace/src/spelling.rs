//! FINDING A PROJECT PATH ON THE REAL DISK ONLY AS THE DISK SPELLS IT.
//!
//! macOS's default disk reads `Lib` as the directory `lib`, and APFS, the
//! case-sensitive kind too, reads `café` in either Unicode normalization as
//! one name. The sandbox tells every spelling apart. Where the disk holds a
//! name only under another spelling, the project overlay's lower layer
//! (`sandbox/spelled_fs.rs`) and the change ledger (`overlay/book.rs`) read
//! the path as absent, so the sandbox shows what applying writes.
//!
//! One `realpath` answers most lookups: on macOS it returns the disk's own
//! spelling. Only when that differs (another spelling, or a symlink on the
//! way), or there is none for a name that is there (a symlink that dangles
//! or loops), are the directories' listings read; each is kept, and read
//! again only once its directory changed. On Linux, `realpath` keeps the
//! spelling it is given, so a case-folding directory there (`chattr +F`)
//! still reads a name the way the disk finds it.

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fmt, io,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long ago a directory must have last changed for its listing to be
/// kept. A disk stamps a change at its own clock's resolution (a whole
/// second on HFS+, two on FAT, a few milliseconds on Linux's coarse clock),
/// so a change right after a read can carry the very stamp the read saw.
const SETTLED: Duration = Duration::from_secs(2);

/// Listings kept at most; past that every one is forgotten and read again.
const KEPT: usize = 4096;

/// A project directory on the real disk, asked how the disk spells a path
/// inside it.
#[derive(Debug, Clone)]
pub(crate) struct Spelling {
    /// The directory as mounted, and as `realpath` spells it; None when it
    /// does not resolve, and then every path reads as asked.
    root: Option<(PathBuf, PathBuf)>,
    /// The listings read on the way to a path, kept while their directory
    /// stays as it was: a lookup reads each one again only once it changed.
    listings: Arc<Listings>,
    /// How long ago a directory must have last changed for its listing to
    /// be kept ([`SETTLED`]).
    settled: Duration,
}

/// How the disk spells a project path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Spelled {
    /// As asked, or the path is not there at all.
    AsAsked,
    /// Under another spelling: that one, as a project path (`/lib/a.rs`), or
    /// None when it resolves outside the project (through a symlink).
    Other(Option<PathBuf>),
}

impl Spelling {
    /// The project at `dir`, resolved once, now. The directory's own spelling
    /// is the human's choice and is never questioned.
    pub(crate) fn of(dir: &Path) -> Self {
        let real = std::fs::canonicalize(dir).ok();
        Self {
            root: real.map(|real| (dir.to_path_buf(), real)),
            listings: Arc::default(),
            settled: SETTLED,
        }
    }

    /// How the disk spells `path`, a project path (`/Lib/a.rs`). A path the
    /// disk cannot resolve (not there, unreadable) reads as asked: the call
    /// that follows answers for it.
    pub(crate) async fn spelled(&self, path: &Path) -> Spelled {
        self.find(path).await.unwrap_or(Spelled::AsAsked)
    }

    /// How the disk spells the way to `path`, a project path that is not
    /// there: the nearest directory above it that is. A write to `path`
    /// lands in that directory, so where the disk holds it under another
    /// spelling (`lib` for `/Lib/new.rs`), the write lands there too.
    pub(crate) async fn spelled_way(&self, path: &Path) -> Spelled {
        let mut at = path.parent();
        while let Some(dir) = at.filter(|d| d.parent().is_some()) {
            if let Some(spelled) = self.find(dir).await {
                return spelled;
            }
            at = dir.parent();
        }
        Spelled::AsAsked
    }

    /// How the disk spells `path`; None when nothing is there by any
    /// spelling.
    async fn find(&self, path: &Path) -> Option<Spelled> {
        let Some((dir, real)) = &self.root else {
            return Some(Spelled::AsAsked);
        };
        let rel = path.strip_prefix("/").unwrap_or(path);
        // Overlay paths come normalized; anything else is the call's to answer.
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Some(Spelled::AsAsked);
        }
        let asked = dir.join(rel);
        let found = match tokio::fs::canonicalize(&asked).await {
            Ok(found) => found,
            Err(e) => {
                // A last name that does not resolve (a symlink that dangles,
                // or one of a loop) is still a name its directory lists, by
                // this spelling or not.
                let Ok(link) = tokio::fs::symlink_metadata(&asked).await else {
                    return (e.kind() != io::ErrorKind::NotFound).then_some(Spelled::AsAsked);
                };
                if self.listed(dir, rel).await {
                    return Some(Spelled::AsAsked);
                }
                return Some(Spelled::Other(link_at(real, &asked, &link).await));
            }
        };
        if found == real.join(rel) || self.listed(dir, rel).await {
            return Some(Spelled::AsAsked);
        }
        let inside = found.strip_prefix(real).ok();
        Some(Spelled::Other(inside.map(|r| Path::new("/").join(r))))
    }

    /// Whether each name in `rel` is one its directory lists, as written. A
    /// directory that cannot be read leaves the answer to the call (true).
    async fn listed(&self, dir: &Path, rel: &Path) -> bool {
        let mut at = dir.to_path_buf();
        for name in rel {
            if !self.lists(&at, name).await {
                return false;
            }
            at.push(name);
        }
        true
    }

    /// Whether `dir` lists `name`. Its listing is read once, and again only
    /// once the directory changed (its ctime moved, or the path leads to
    /// another directory): so a tree of N files under a symlink costs N
    /// lookups, not N listings of N names.
    async fn lists(&self, dir: &Path, name: &OsStr) -> bool {
        let now = SystemTime::now();
        let Ok(stamp) = tokio::fs::metadata(dir).await.map(|m| Stamp::of(&m)) else {
            return true;
        };
        if let Some(names) = self.listings.kept(dir, stamp) {
            return names.contains(name);
        }
        let Some(names) = self.listings.read(dir).await else {
            return true;
        };
        let listed = names.contains(name);
        if stamp.settled(now, self.settled) {
            self.listings.keep(dir, stamp, names);
        }
        listed
    }
}

/// The names one directory lists.
type Names = Arc<HashSet<OsString>>;

/// Directory listings, by the directory's path as asked.
#[derive(Default)]
struct Listings {
    kept: Mutex<HashMap<PathBuf, Listing>>,
    /// Listings read from the disk, for the tests.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
}

/// One directory's names, and the stamp of the directory they were read
/// from.
struct Listing {
    stamp: Stamp,
    names: Names,
}

impl Listings {
    /// `dir`'s listing, if one is kept and `dir` is still as it was then.
    fn kept(&self, dir: &Path, now: Stamp) -> Option<Names> {
        let kept = self.lock();
        let listing = kept.get(dir)?;
        (listing.stamp == now).then(|| listing.names.clone())
    }

    fn keep(&self, dir: &Path, stamp: Stamp, names: Names) {
        let mut kept = self.lock();
        if kept.len() >= KEPT {
            kept.clear();
        }
        kept.insert(dir.to_path_buf(), Listing { stamp, names });
    }

    /// Every name `dir` lists, from the disk; None when it cannot be read.
    async fn read(&self, dir: &Path) -> Option<Names> {
        #[cfg(test)]
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut entries = tokio::fs::read_dir(dir).await.ok()?;
        let mut names = HashSet::new();
        while let Some(e) = entries.next_entry().await.ok()? {
            names.insert(e.file_name());
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
struct Stamp {
    dev: u64,
    ino: u64,
    changed: (i64, i64),
}

impl Stamp {
    fn of(m: &std::fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }

    /// Whether the directory last changed at least `settled` before `now`.
    fn settled(&self, now: SystemTime, settled: Duration) -> bool {
        let (Ok(secs), Ok(nanos)) = (u64::try_from(self.changed.0), u32::try_from(self.changed.1))
        else {
            return false;
        };
        let changed = UNIX_EPOCH.checked_add(Duration::new(secs, nanos));
        changed.is_some_and(|c| now.duration_since(c).is_ok_and(|age| age >= settled))
    }
}

/// Where the disk holds `asked`, a symlink that does not resolve, as a
/// project path: its directory as `realpath` spells it, and its own name as
/// that directory lists the same link. None when that cannot be told, or is
/// outside the project `real`.
async fn link_at(real: &Path, asked: &Path, link: &std::fs::Metadata) -> Option<PathBuf> {
    let dir = tokio::fs::canonicalize(asked.parent()?).await.ok()?;
    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    while let Some(e) = entries.next_entry().await.ok()? {
        if e.ino() == link.ino() {
            let inside = dir.strip_prefix(real).ok()?;
            return Some(Path::new("/").join(inside).join(e.file_name()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_path_reads_as_asked_only_by_the_disks_own_spelling() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        std::fs::create_dir(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/caf\u{e9}"), "").unwrap();
        std::os::unix::fs::symlink("lib", dir.join("link")).unwrap();
        let s = Spelling::of(dir);
        let asked_as_spelled = [
            "/",
            "/lib",
            "/lib/caf\u{e9}",
            "/link/caf\u{e9}",
            "/nope",
            "/lib/../x",
        ];
        for asked in asked_as_spelled {
            let got = s.spelled(Path::new(asked)).await;
            assert_eq!(got, Spelled::AsAsked, "{asked}");
        }
        // Each other spelling, where this disk reads it as the same name.
        for asked in ["/LIB/caf\u{e9}", "/lib/cafe\u{301}"] {
            let want = if dir.join(&asked[1..]).exists() {
                Spelled::Other(Some(PathBuf::from("/lib/caf\u{e9}")))
            } else {
                Spelled::AsAsked
            };
            assert_eq!(s.spelled(Path::new(asked)).await, want, "{asked}");
        }
    }

    impl Spelling {
        /// Listings read from the disk so far.
        fn reads(&self) -> usize {
            self.listings
                .reads
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// `store` with `n` files, and `vendor`, a symlink to it, in `dir`.
    fn linked_store(dir: &Path, n: usize) {
        std::fs::create_dir(dir.join("store")).unwrap();
        for i in 0..n {
            std::fs::write(dir.join(format!("store/f{i}")), "").unwrap();
        }
        std::os::unix::fs::symlink("store", dir.join("vendor")).unwrap();
    }

    /// Under a symlinked directory `realpath` never matches the path asked,
    /// so a lookup reads the listings on its way. Each lookup read them all
    /// again: `grep -r` over N files read N listings of N names, and over
    /// 8,000 files took 26 s where 1 s would do.
    #[tokio::test]
    async fn lookups_under_a_symlink_read_each_listing_once_until_it_changes() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        linked_store(dir, 100);
        // A disk with an exact clock: what was just made is kept at once.
        let s = Spelling {
            settled: Duration::ZERO,
            ..Spelling::of(dir)
        };
        for i in 0..100 {
            let asked = format!("/vendor/f{i}");
            assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
        }
        // The project's and the store's.
        assert_eq!(s.reads(), 2);
        // A change in the store is seen: its listing is read again. (Past a
        // coarse clock's tick first, so the change moves the stamp.)
        std::thread::sleep(Duration::from_millis(50));
        std::fs::rename(dir.join("store/f0"), dir.join("store/F0")).unwrap();
        for asked in ["/vendor/F0", "/vendor/f1"] {
            assert_eq!(
                s.spelled(Path::new(asked)).await,
                Spelled::AsAsked,
                "{asked}"
            );
        }
        assert_eq!(s.reads(), 3);
    }

    /// A directory changed within [`SETTLED`] of its listing's read may
    /// change again with the same stamp: its listing is read at every
    /// lookup until it settles.
    #[tokio::test]
    async fn a_listing_is_kept_only_once_its_directory_settled() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        linked_store(dir, 3);
        let s = Spelling::of(dir);
        for i in 0..3 {
            let asked = format!("/vendor/f{i}");
            assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
        }
        assert_eq!(s.reads(), 6);
    }

    /// A symlink that dangles or loops has no `realpath`; it is found by its
    /// directory's listing, and its place by the entry that is the same link.
    #[tokio::test]
    async fn a_link_that_does_not_resolve_is_found_only_as_the_disk_spells_it() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        std::fs::create_dir(dir.join("lib")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.join("lib/d")).unwrap();
        std::os::unix::fs::symlink("l2", dir.join("lib/l1")).unwrap();
        std::os::unix::fs::symlink("l1", dir.join("lib/l2")).unwrap();
        let s = Spelling::of(dir);
        for asked in ["/lib/d", "/lib/l1", "/lib/l2"] {
            assert_eq!(
                s.spelled(Path::new(asked)).await,
                Spelled::AsAsked,
                "{asked}"
            );
        }
        for (asked, disk) in [
            ("/LIB/d", "/lib/d"),
            ("/lib/D", "/lib/d"),
            ("/lib/L1", "/lib/l1"),
        ] {
            let folds = std::fs::symlink_metadata(dir.join(&asked[1..])).is_ok();
            let want = if folds {
                Spelled::Other(Some(PathBuf::from(disk)))
            } else {
                Spelled::AsAsked
            };
            assert_eq!(s.spelled(Path::new(asked)).await, want, "{asked}");
        }
    }

    /// A path that is not there lands in the nearest directory above it that
    /// is, as the disk spells that one.
    #[tokio::test]
    async fn the_way_to_a_missing_path_is_its_nearest_directory_there() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        std::fs::create_dir(dir.join("lib")).unwrap();
        let s = Spelling::of(dir);
        for asked in ["/lib/new.rs", "/lib/new/deeper/x.rs", "/nope/x.rs", "/x.rs"] {
            let got = s.spelled_way(Path::new(asked)).await;
            assert_eq!(got, Spelled::AsAsked, "{asked}");
        }
        let want = if dir.join("LIB").exists() {
            Spelled::Other(Some(PathBuf::from("/lib")))
        } else {
            Spelled::AsAsked
        };
        for asked in ["/LIB/new.rs", "/LIB/new/deeper/x.rs"] {
            assert_eq!(s.spelled_way(Path::new(asked)).await, want, "{asked}");
        }
    }

    #[tokio::test]
    async fn a_directory_that_does_not_resolve_reads_every_path_as_asked() {
        let s = Spelling::of(Path::new("/no/such/dir"));
        assert_eq!(s.spelled(Path::new("/LIB")).await, Spelled::AsAsked);
    }
}
