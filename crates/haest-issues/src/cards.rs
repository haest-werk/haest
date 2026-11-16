//! Board cards as a [`Tracker`]: one markdown file per card, `<id>.md` in one
//! directory, its frontmatter the card's fields and its body the description.
//!
//! | Tracker | Card |
//! |---|---|
//! | id | the file name without `.md`, derived from the title at create and never changed |
//! | status | `status:` (inbox, open, in-progress, in-review, done, archived); the file never moves |
//! | priority | `priority:` (low, medium, high) |
//! | labels | `tags:` |
//! | parent / child | `epic:` on the child |
//! | depends_on / blocks | `depends_on:` on the one that waits |
//! | relates_to | `relates_to:` (read both ways) |
//!
//! Every key the card has survives a write, unknown ones included, so a card
//! another program wrote keeps what that program put there. The directory and
//! the file system are the host's: nothing here picks a path.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use haest_core::{
    __private::slug,
    Clock, WallClock,
    fs::{FileKind, Fs, ensure_dir},
    session::iso_utc,
};
use tokio::sync::Mutex;

use crate::tracker::{
    Issue, IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, Tracker, TrackerError,
    TrackerInfo, one_of,
};

mod card;
mod front;
mod links;

use card::{
    ID_MAX, OPEN_STATUSES, PRIORITIES, STATUSES, as_strings, check_id, found_by, issue_of,
    status_of,
};
use front::{Front, Val};

/// A [`Tracker`] over a directory of markdown cards (the [module
/// docs](self) map each field to its frontmatter key).
///
/// - **Statuses** inbox, open, in-progress, in-review, done, archived. A new
///   card starts in the inbox; a list with no statuses given shows the first
///   four. A card whose `status:` is none of these reads as inbox.
/// - **Priorities** low, medium, high. **Relations** all of [`Relation::ALL`].
/// - **Ids** are the title as a slug of ASCII letters, digits and dashes, at
///   most 60 characters (`Fix the login` is `fix-the-login`), with `-2`,
///   `-3`... when the name is taken, and `task-<ms>` for a title with nothing
///   to slug. An id is never a path: one with a `/` or `\`, a leading dot or
///   a control character is refused.
/// - **Lists and searches** come newest first. A search finds the cards whose
///   id, title, tags and body hold every word, case ignored.
/// - **Writes** go to a hidden file beside the card, renamed over it, so a
///   reader never sees half a card; and one tracker writes one card at a
///   time. Two trackers over the same directory (or another program) do not
///   wait for each other.
/// - **Links** live on one card and read from both (`blocks` is the other
///   card's `depends_on`). A card cannot link to itself, and a parent or
///   dependency that would make a loop is refused.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::{Fs, MemFs};
/// use haest_issues::{Link, LinkChange, NewIssue, Relation, Tracker, cards::CardTracker};
///
/// # #[tokio::main]
/// # async fn main() {
/// let fs = Arc::new(MemFs::default());
/// let board = CardTracker::new(fs.clone(), "/project/cards");
///
/// let new = |title: &str| NewIssue { title: title.into(), ..NewIssue::default() };
/// let login = board.create(new("Fix the login")).await.unwrap();
/// let tests = board.create(new("Add login tests")).await.unwrap();
/// assert!(fs.exists("/project/cards/fix-the-login.md".as_ref()).await);
///
/// // The tests wait on the fix; the fix then reads as blocking them.
/// let waits = Link::new(Relation::DependsOn, &login.id);
/// board.link(&tests.id, &waits, LinkChange::Add).await.unwrap();
/// let fix = board.get(&login.id).await.unwrap();
/// assert!(fix.links.contains(&Link::new(Relation::Blocks, &tests.id)));
///
/// // The other way round would be a loop.
/// let back = Link::new(Relation::DependsOn, &tests.id);
/// assert!(board.link(&login.id, &back, LinkChange::Add).await.is_err());
/// # }
/// ```
pub struct CardTracker {
    fs: Arc<dyn Fs>,
    dir: PathBuf,
    clock: Box<dyn Clock>,
    /// One write at a time: a read-change-write must not lose another's change.
    writes: Mutex<()>,
}

impl CardTracker {
    /// The cards in `dir`, read and written through `fs` (the host's: a
    /// sandbox's, the real disk). The directory is made on the first write;
    /// until then the board is empty. Nothing is read here.
    pub fn new(fs: Arc<dyn Fs>, dir: impl Into<PathBuf>) -> Self {
        Self {
            fs,
            dir: dir.into(),
            clock: Box::new(WallClock),
            writes: Mutex::new(()),
        }
    }

    /// The clock a new card's `created:` (and a fallback id) comes from; the
    /// [`WallClock`] by default. A test passes its own to fix the dates (any
    /// `Fn() -> u64` is a [`Clock`]).
    pub fn with_clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Box::new(clock);
        self
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
            let _ = self.fs.remove(&temp, false).await;
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

fn parse_card(text: &str) -> Front {
    Front::parse(text).unwrap_or_else(|| Front {
        keys: Vec::new(),
        body: text.trim().to_string(),
    })
}

/// Newest first; a card with no `created:` last; then by id.
fn sorted(mut issues: Vec<Issue>, limit: usize) -> Vec<Issue> {
    issues.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
    issues.truncate(limit);
    issues
}

#[async_trait]
impl Tracker for CardTracker {
    fn info(&self) -> TrackerInfo {
        TrackerInfo {
            name: format!("the board cards in {}", self.dir.display()),
            id_hint: "the card id (its file name without .md), e.g. \"fix-login\"".into(),
            statuses: as_strings(STATUSES),
            open_statuses: as_strings(OPEN_STATUSES),
            priorities: as_strings(PRIORITIES),
            relations: Relation::ALL.to_vec(),
        }
    }

    async fn list(&self, filter: &IssueFilter) -> Result<Vec<Issue>, TrackerError> {
        let wanted = (filter.statuses.clone()).unwrap_or_else(|| as_strings(OPEN_STATUSES));
        for s in &wanted {
            one_of(s, &as_strings(STATUSES), "status")?;
        }
        if let Some(p) = &filter.priority {
            one_of(p, &as_strings(PRIORITIES), "priority")?;
        }
        let issues = (self.all().await?.iter())
            .map(|(id, f)| self.issue(id, f))
            .filter(|i| wanted.contains(&i.status))
            .filter(|i| filter.labels.iter().all(|l| i.labels.contains(l)))
            .filter(|i| filter.priority.is_none() || i.priority == filter.priority)
            .collect();
        Ok(sorted(issues, filter.limit))
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Issue>, TrackerError> {
        if query.trim().is_empty() {
            return Err(TrackerError::Invalid("the search has no words".into()));
        }
        let issues = (self.all().await?.iter())
            .map(|(id, f)| self.issue(id, f))
            .filter(|i| found_by(i, query))
            .collect();
        Ok(sorted(issues, limit))
    }

    async fn get(&self, id: &str) -> Result<Issue, TrackerError> {
        let id = check_id(id).map_err(TrackerError::Invalid)?;
        let front = self.read(id).await?;
        let mut issue = self.issue(id, &front);
        issue
            .links
            .extend(links::inverse_links(id, &front, &self.all().await?));
        Ok(issue)
    }

    async fn create(&self, new: NewIssue) -> Result<Issue, TrackerError> {
        let title = new.title.trim();
        if title.is_empty() {
            return Err(TrackerError::Invalid("a card needs a title".into()));
        }
        let status = new.status.unwrap_or_else(|| STATUSES[0].into());
        one_of(&status, &as_strings(STATUSES), "status")?;
        if let Some(p) = &new.priority {
            one_of(p, &as_strings(PRIORITIES), "priority")?;
        }
        let _one_writer = self.writes.lock().await;
        let now = self.clock.now_ms();
        let id = self.new_id(title, now).await;
        let mut front = Front {
            keys: Vec::new(),
            body: new.body.trim().to_string(),
        };
        front.set("title", Val::Str(title.into()));
        front.set("status", Val::Str(status));
        if let Some(p) = new.priority {
            front.set("priority", Val::Str(p));
        }
        front.set_list("tags", new.labels);
        front.set("created", Val::Str(iso_utc(now)));
        self.write(&id, &front).await?;
        Ok(self.issue(&id, &front))
    }

    async fn update(&self, id: &str, patch: IssuePatch) -> Result<Issue, TrackerError> {
        if patch.is_empty() {
            return Err(TrackerError::Invalid("the update changes nothing".into()));
        }
        if let Some(s) = &patch.status {
            one_of(s, &as_strings(STATUSES), "status")?;
        }
        if let Some(p) = &patch.priority {
            one_of(p, &as_strings(PRIORITIES), "priority")?;
        }
        let id = check_id(id).map_err(TrackerError::Invalid)?;
        let _one_writer = self.writes.lock().await;
        let mut front = self.read(id).await?;
        if let Some(title) = patch.title.as_deref().map(str::trim) {
            if title.is_empty() {
                return Err(TrackerError::Invalid("a card needs a title".into()));
            }
            front.set("title", Val::Str(title.into()));
        }
        if let Some(body) = &patch.body {
            front.body = body.trim().to_string();
        }
        // Every write leaves a lane the board reads: an unknown one becomes the inbox.
        let status = (patch.status.clone()).unwrap_or_else(|| status_of(&front).to_string());
        front.set("status", Val::Str(status));
        if let Some(p) = patch.priority.clone() {
            front.set("priority", Val::Str(p));
        }
        if patch.changes_labels() {
            let tags = patch.labels_from(&front.list("tags"));
            front.set_list("tags", tags);
        }
        self.write(id, &front).await?;
        Ok(self.issue(id, &front))
    }

    async fn delete(&self, id: &str) -> Result<(), TrackerError> {
        let id = check_id(id).map_err(TrackerError::Invalid)?;
        let _one_writer = self.writes.lock().await;
        let path = self.path(id);
        if !self.fs.exists(&path).await {
            return Err(TrackerError::NotFound(format!("there is no card \"{id}\"")));
        }
        (self.fs.remove(&path, false).await).map_err(|e| self.failed("delete", &path, e))
    }

    async fn link(&self, id: &str, link: &Link, change: LinkChange) -> Result<(), TrackerError> {
        self.change_link(id, link, change).await
    }
}

#[cfg(test)]
mod tests;
