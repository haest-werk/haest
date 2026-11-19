//! FINDING A PROJECT PATH ON THE REAL DISK ONLY AS THE DISK SPELLS IT.
//!
//! macOS's default disk reads `Lib` as the directory `lib`, and APFS, the
//! case-sensitive kind too, reads `café` in either Unicode normalization as
//! one name; so do exFAT and FAT, and vfat on Linux. The sandbox tells every
//! spelling apart. Where the disk holds a name only under another spelling,
//! the project overlay's lower layer (`sandbox/spelled_fs.rs`) and the
//! change ledger (`overlay/book.rs`) read the path as absent, so the
//! sandbox shows what applying writes.
//!
//! A path is there as asked where each name in it is one its directory
//! lists. One `realpath` answers that for most lookups, where it gives back
//! each name as asked on a disk that answers with the name it holds (APFS,
//! HFS+), or in directories that tell cases apart (`answers.rs`), each
//! known as one once (`folding.rs`); where only a case swap told that, for
//! an ASCII name alone. Elsewhere (exFAT and FAT answer with
//! the spelling a file was first looked up by, vfat and a case-folding
//! directory on Linux with the one asked), in a directory not known yet,
//! and where it spells a name another way (another spelling, or a symlink
//! on the way) or finds none for a name that is there (a symlink that
//! dangles or loops), the directories' listings decide; each is kept, and
//! read again only once its directory changed, or, at the root of a mount,
//! for a name it does not hold. The disk's other spelling
//! of a name is the entry its directory lists for the same file
//! ([`crate::listed`]), never `realpath`'s.

mod answers;
mod disks;
mod folding;
mod listings;

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use answers::{Answers, Resolved, folds_by_case, has_letter, resolve};
use folding::{Dir, Folding};
use listings::{Listings, Names, SETTLED, Stamp};

use crate::listed::{confirmed, pick};

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
    /// The directories known to read two spellings of a name as one or
    /// not, and what each disk said.
    folding: Arc<Folding>,
    /// How long ago a directory must have last changed for its listing to
    /// be kept ([`SETTLED`]).
    settled: Duration,
    /// How the disk answers before any listing is read.
    answers: Answers,
    /// A test's answer for some paths, in place of the disk's: the answer
    /// made wrong on purpose ([`Spelling::told`]).
    #[cfg(test)]
    told: Option<fn(&Path) -> Option<Spelled>>,
}

/// What [`Spelling::spelled_way`] found for each directory it asked: the
/// files under one new directory all ask the same ones. Kept only while the
/// disk is taken to stand still, so for one scan, never across writes.
#[derive(Debug, Default)]
pub(crate) struct Ways(HashMap<PathBuf, Option<Spelled>>);

/// How the disk spells a project path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Spelled {
    /// As asked, or the path is not there at all.
    AsAsked,
    /// Under another spelling: that one, as a project path (`/lib/a.rs`),
    /// each name as its directory lists it; None where a name's entry
    /// cannot be told.
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
            folding: Arc::default(),
            settled: SETTLED,
            answers: Answers::SYSTEM,
            #[cfg(test)]
            told: None,
        }
    }

    /// Its answer for a path is `told`'s where that gives one: wrong on
    /// purpose, "as asked" for a name the disk holds under another
    /// spelling, or "another spelling" for one that is there as asked.
    #[cfg(test)]
    pub(crate) fn told(mut self, told: fn(&Path) -> Option<Spelled>) -> Self {
        self.told = Some(told);
        self
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
    ///
    /// `ways` keeps what each directory answered, for the lookups that
    /// follow while the disk is taken to stand still (one scan).
    pub(crate) async fn spelled_way(&self, path: &Path, ways: &mut Ways) -> Spelled {
        let mut at = path.parent();
        while let Some(dir) = at.filter(|d| d.parent().is_some()) {
            let found = if let Some(found) = ways.0.get(dir) {
                found.clone()
            } else {
                let found = self.find(dir).await;
                ways.0.insert(dir.to_path_buf(), found.clone());
                found
            };
            if let Some(spelled) = found {
                return spelled;
            }
            at = dir.parent();
        }
        Spelled::AsAsked
    }

    /// How the disk spells `path`; None when nothing is there by any
    /// spelling.
    async fn find(&self, path: &Path) -> Option<Spelled> {
        #[cfg(test)]
        if let Some(told) = self.told.and_then(|told| told(path)) {
            return Some(told);
        }
        let Some((dir, real)) = &self.root else {
            return Some(Spelled::AsAsked);
        };
        let inside = path.strip_prefix("/").unwrap_or(path);
        // Overlay paths come normalized; anything else is the call's to answer.
        if !inside
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Some(Spelled::AsAsked);
        }
        let (answers, folding) = (self.answers, self.folding.clone());
        let at = [dir.clone(), real.clone(), inside.to_path_buf()];
        let resolved = tokio::task::spawn_blocking(move || {
            let [dir, real, inside] = &at;
            resolve(answers, &folding, dir, real, inside)
        })
        .await
        .unwrap_or(Resolved::Unknown);
        match resolved {
            Resolved::Absent => None,
            Resolved::Unknown | Resolved::Listed => Some(Spelled::AsAsked),
            Resolved::Unsure if self.listed(dir, inside).await => Some(Spelled::AsAsked),
            Resolved::Unsure => Some(Spelled::Other(self.listed_at(dir, inside).await)),
        }
    }

    /// Whether each name in `inside` is one its directory lists, as written.
    /// A directory that cannot be read leaves the answer to the call (true).
    async fn listed(&self, dir: &Path, inside: &Path) -> bool {
        let mut at = dir.to_path_buf();
        for name in inside {
            if !self.lists(&at, name).await {
                return false;
            }
            at.push(name);
        }
        true
    }

    /// Whether `dir` lists `name`.
    async fn lists(&self, dir: &Path, name: &OsStr) -> bool {
        self.listing(dir, name)
            .await
            .is_none_or(|names| names.contains_key(name))
    }

    /// Where the disk holds `inside`, which is not listed as asked: each
    /// name as its directory lists the same file, as a project path. None
    /// where one cannot be told.
    async fn listed_at(&self, dir: &Path, inside: &Path) -> Option<PathBuf> {
        let (mut at, mut out) = (dir.to_path_buf(), PathBuf::from("/"));
        for name in inside {
            let listed = self.listed_name(&at, name).await?;
            at.push(&listed);
            out.push(&listed);
        }
        Some(out)
    }

    /// The name `dir` lists the file asked for as `name` by: that one, or
    /// the entry `lstat` agrees is the same file ([`crate::listed`]).
    async fn listed_name(&self, dir: &Path, name: &OsStr) -> Option<OsString> {
        let names = self.listing(dir, name).await?;
        if names.contains_key(name) {
            return Some(name.to_os_string());
        }
        let own = tokio::fs::symlink_metadata(dir.join(name)).await.ok()?;
        let listing = names.iter().map(|(n, ino)| Ok::<_, io::Error>((n, *ino)));
        let picked = pick(name, own.ino(), listing);
        confirmed(dir, name, picked, (own.dev(), own.ino())).await
    }

    /// As on Linux's ext4 or tmpfs: no disk answers `realpath` with the
    /// name it holds, and each directory's own flag says it tells case
    /// apart.
    #[cfg(test)]
    pub(crate) fn telling_case_apart(mut self) -> Self {
        self.answers.disk = |_| folding::Disk::Flagged;
        self.answers.flag = |_| Some(false);
        self
    }

    /// Listings read from the disk so far.
    #[cfg(test)]
    pub(crate) fn reads(&self) -> usize {
        self.listings
            .reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The names `dir` lists, asked for `name`; None when it cannot be
    /// read. Read once, and again only once the directory changed (its
    /// ctime moved, or the path leads to another directory): so a tree of N
    /// files under a symlink costs N lookups, not N listings of N names.
    ///
    /// At the root of a mount the stamp may never move: the root of an
    /// exFAT, FAT or vfat volume keeps a ctime of 0 (exFAT on macOS, vfat)
    /// or of its mount (FAT on macOS) whatever is added there. Its listing
    /// answers only for the names it holds; a name it lacks, made there
    /// since, reads it again. A name it holds that is gone is absent to
    /// `lstat` before any listing is asked. One the human renamed there by
    /// case or Unicode form alone reads as there by its old name until a
    /// lookup reads the root again; the apply takes nothing by that name,
    /// as its own check reads the listing afresh (`overlay/shown.rs`).
    async fn listing(&self, dir: &Path, name: &OsStr) -> Option<Names> {
        let now = SystemTime::now();
        let m = tokio::fs::metadata(dir).await.ok()?;
        let stamp = Stamp::of(&m);
        if let Some(names) = self.listings.kept(dir, stamp, name) {
            return Some(names);
        }
        let names = self.listings.read(dir).await?;
        if stamp.settled(now, self.settled) {
            let complete = !self.mount_root(dir, &m).await;
            self.listings.keep(dir, stamp, names.clone(), complete);
        }
        self.learn(dir, Dir::of(&m), &names).await;
        Some(names)
    }

    /// Whether the directory `dir`, whose metadata is `m`, is the root of a
    /// mount ([`Answers::mount_root`]); asked off the runtime's workers.
    async fn mount_root(&self, dir: &Path, m: &std::fs::Metadata) -> bool {
        let (mount_root, dir, m) = (self.answers.mount_root, dir.to_path_buf(), m.clone());
        tokio::task::spawn_blocking(move || mount_root(&dir, &m))
            .await
            // The answer was lost: nothing says its stamp moves.
            .unwrap_or(true)
    }

    /// Whether `dir` (`id`) reads two spellings of a name as one, from one
    /// entry of its listing `names` that has an ASCII letter, where nothing
    /// told that yet: the names asked there had none (`00001`, `2024`).
    /// With none listed either, it stays unknown, and its listing decides.
    async fn learn(&self, dir: &Path, id: Dir, names: &Names) {
        if !self.folding.undecided(id) {
            return;
        }
        let Some(name) = names.keys().find(|n| has_letter(n)) else {
            return;
        };
        let (dir, name) = (dir.to_path_buf(), name.clone());
        let told = tokio::task::spawn_blocking(move || folds_by_case(&dir, &name))
            .await
            .ok()
            .flatten();
        if let Some(folds) = told {
            self.folding.decide(id, folds);
        }
    }
}

#[cfg(test)]
mod tests;
