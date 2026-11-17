//! [`CardTracker`]: board cards as a [`Tracker`](crate::Tracker).

use std::{
    fmt,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use haest_core::{
    ClockRef, WallClock,
    fs::{FileKind, FsRef, ensure_dir},
    tool::slug,
};
use tokio::sync::Mutex;

use crate::tracker::{Issue, TrackerError};

mod calls;
mod card;
mod front;
mod links;

use card::{ID_MAX, check_id, issue_of};
use front::Front;

/// A [`Tracker`](crate::Tracker) over a directory of markdown cards: one
/// file per card, `<id>.md` in one directory, its frontmatter the card's
/// fields and its body the description.
///
/// | Tracker | Card |
/// |---|---|
/// | id | the file name without `.md`, derived from the title at create and never changed |
/// | status | `status:` (inbox, open, in-progress, in-review, done, archived); the file never moves |
/// | priority | `priority:` (low, medium, high) |
/// | labels | `tags:` |
/// | parent / child | `epic:` on the child |
/// | depends_on / blocks | `depends_on:` on the one that waits |
/// | relates_to | `relates_to:` (read both ways) |
///
/// Every key the card has survives a write, unknown ones included, so a card
/// another program wrote keeps what that program put there. The directory and
/// the file system are the host's: nothing here picks a path.
///
/// - **Statuses** inbox, open, in-progress, in-review, done, archived. A new
///   card starts in the inbox; a list with no statuses given shows the first
///   four. A card whose `status:` is none of these reads as inbox.
/// - **Priorities** low, medium, high. **Relations** all of
///   [`Relation::ALL`](crate::Relation::ALL).
/// - **Ids** are the title as a slug of ASCII letters, digits and dashes, at
///   most 60 characters (`Fix the login` is `fix-the-login`), with `-2`,
///   `-3`... when the name is taken, and `task-<ms>` for a title with nothing
///   to slug. An id is never a path: one with a `/` or `\`, a leading dot or
///   a control character is refused.
/// - **Lists and searches** come newest first. A search finds the cards whose
///   id, title, tags and body hold every word, case ignored.
/// - **Writes** go to a hidden file beside the card, renamed over it, so a
///   reader never sees half a card; and one tracker writes one card at a
///   time. The lock is the tracker's (its clones share it), not the
///   directory's: two trackers made apart over the same directory (or
///   another program) do not wait for each other, so two writes at the same
///   moment can lose one's change, and two cards made with the same title can
///   end up as one. Make one tracker per directory and hand clones of it to
///   everything that writes there; cloning is cheap.
/// - **Links** live on one card and read from both (`blocks` is the other
///   card's `depends_on`). A card cannot link to itself, and a parent or
///   dependency that would make a loop is refused.
///
/// # Examples
///
/// ```
/// use haest_core::{FsRef, MemFs};
/// use haest_issues::{CardTracker, Link, LinkChange, NewIssue, Relation, Tracker, TrackerError};
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), TrackerError> {
/// // The host keeps a handle to the file system the board writes through.
/// let fs = FsRef::new(MemFs::default());
/// let board = CardTracker::new("/project/cards", fs.clone());
///
/// let new = |title: &str| NewIssue { title: title.into(), ..NewIssue::default() };
/// let login = board.create(new("Fix the login")).await?;
/// let tests = board.create(new("Add login tests")).await?;
/// assert!(fs.exists("/project/cards/fix-the-login.md".as_ref()).await);
///
/// // The tests wait on the fix; the fix then reads as blocking them.
/// let waits = Link::new(Relation::DependsOn, &login.id);
/// board.link(&tests.id, &waits, LinkChange::Add).await?;
/// let fix = board.get(&login.id).await?;
/// assert!(fix.links.contains(&Link::new(Relation::Blocks, &tests.id)));
///
/// // The other way round would be a loop.
/// let back = Link::new(Relation::DependsOn, &tests.id);
/// assert!(board.link(&login.id, &back, LinkChange::Add).await.is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct CardTracker {
    fs: FsRef,
    dir: Arc<Path>,
    clock: ClockRef,
    /// One write at a time: a read-change-write must not lose another's
    /// change. This tracker's writes (and its clones') only; see the type's
    /// docs.
    writes: Arc<Mutex<()>>,
}

impl CardTracker {
    /// The cards in `dir`, read and written through `fs` (the host's: a
    /// sandbox's, the real disk; the file system itself or an `Arc` of one).
    /// The directory is made on the first write; until then the board is
    /// empty. Nothing is read here.
    pub fn new(dir: impl Into<PathBuf>, fs: impl Into<FsRef>) -> Self {
        Self {
            fs: fs.into(),
            dir: dir.into().into(),
            clock: ClockRef::new(WallClock),
            writes: Arc::default(),
        }
    }

    /// The clock a new card's `created:` (and a fallback id) comes from; the
    /// [`WallClock`] by default. A test passes its own to fix the dates (any
    /// `Fn() -> u64` is a [`Clock`](haest_core::Clock)).
    #[must_use]
    pub fn with_clock(mut self, clock: impl Into<ClockRef>) -> Self {
        self.clock = clock.into();
        self
    }

    /// The directory the cards are in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.md"))
    }

    fn failed(&self, what: &str, path: &Path, e: std::io::Error) -> TrackerError {
        TrackerError::Failed(format!("could not {what} {}: {e}", path.display()))
    }

    /// The card `id` (a checked id): its frontmatter, or a card of body alone
    /// when it has none. Bytes that are not UTF-8 read as U+FFFD.
    async fn read(&self, id: &str) -> Result<Front, TrackerError> {
        let path = self.path(id);
        match self.fs.read(&path).await {
            Ok(bytes) => Ok(parse_card(&String::from_utf8_lossy(&bytes))),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                Err(TrackerError::NotFound(format!("there is no card \"{id}\"")))
            }
            Err(e) => Err(self.failed("read", &path, e)),
        }
    }

    /// Every card, by id, in id order. No directory yet is no cards.
    async fn all(&self) -> Result<Vec<(String, Front)>, TrackerError> {
        let entries = match self.fs.read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(self.failed("list", &self.dir, e)),
        };
        let mut cards = Vec::new();
        for entry in entries {
            let Some(id) = entry.name.strip_suffix(".md") else {
                continue;
            };
            // A name that is no card id (a hidden file, a temp file) is no card.
            if entry.kind == FileKind::Dir || check_id(id) != Ok(id) {
                continue;
            }
            cards.push((id.to_string(), self.read(id).await?));
        }
        cards.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(cards)
    }

    /// Write the card whole: to a hidden file beside it, then renamed over it,
    /// so a reader never sees half a card.
    async fn write(&self, id: &str, front: &Front) -> Result<(), TrackerError> {
        ensure_dir(self.fs.as_ref(), &self.dir)
            .await
            .map_err(|e| self.failed("make", &self.dir, e))?;
        let path = self.path(id);
        let temp = self.dir.join(format!(".{id}.md.tmp"));
        let written = self.fs.write(&temp, front.render().as_bytes()).await;
        let renamed = match written {
            Ok(()) => self.fs.rename(&temp, &path).await,
            Err(e) => Err(e),
        };
        if let Err(e) = renamed {
            let _ = self.fs.remove(&temp).await;
            return Err(self.failed("write", &path, e));
        }
        Ok(())
    }

    fn issue(&self, id: &str, front: &Front) -> Issue {
        issue_of(id, front, self.path(id).display().to_string())
    }

    /// A new card's id: its title as a slug, `-2`, `-3`... when taken.
    async fn new_id(&self, title: &str, now: u64) -> String {
        let base = slug(title, ID_MAX, &format!("task-{now}"));
        let taken = |id: String| async move { self.fs.exists(&self.path(&id)).await };
        if !taken(base.clone()).await {
            return base;
        }
        for n in 2..100 {
            let id = format!("{base}-{n}");
            if !taken(id.clone()).await {
                return id;
            }
        }
        format!("{base}-{now}")
    }
}

impl fmt::Debug for CardTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CardTracker")
            .field("dir", &self.dir)
            .field("fs", &self.fs)
            .finish_non_exhaustive()
    }
}

fn parse_card(text: &str) -> Front {
    Front::parse(text).unwrap_or_else(|| Front {
        keys: Vec::new(),
        body: text.trim().to_string(),
    })
}

#[cfg(test)]
mod tests;
