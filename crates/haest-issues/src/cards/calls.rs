//! The [`Tracker`](crate::Tracker) calls on a board of cards: each reads the
//! cards it needs, and a write changes one card under the tracker's lock.

use async_trait::async_trait;
use haest_core::session::iso_utc;

use super::{
    CardTracker,
    card::{OPEN_STATUSES, PRIORITIES, STATUSES, as_strings, check_id, found_by, status_of},
    front::{Front, Val},
    links::inverse_links,
};
use crate::tracker::{
    Issue, IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, Tracker, TrackerError,
    TrackerInfo, one_of,
};

/// Newest first; a card with no `created:` last; then by id.
fn sorted(mut issues: Vec<Issue>, limit: usize) -> Vec<Issue> {
    issues.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
    issues.truncate(limit);
    issues
}

#[async_trait]
impl Tracker for CardTracker {
    /// What it is and accepts: the board's lanes, priorities and relations.
    fn info(&self) -> TrackerInfo {
        TrackerInfo::new(
            format!("the board cards in {}", self.dir.display()),
            "the card id (its file name without .md), e.g. \"fix-login\"",
            STATUSES,
        )
        .with_open_statuses(OPEN_STATUSES)
        .with_priorities(PRIORITIES)
        .with_relations(Relation::ALL.iter().copied())
    }

    /// The cards `filter` picks, newest first.
    ///
    /// # Errors
    ///
    /// `Invalid` for a status or a priority the board does not have,
    /// `Failed` when the board cannot be read.
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

    /// The cards in any lane whose title, body or tags hold `query`, newest
    /// first.
    ///
    /// # Errors
    ///
    /// `Invalid` for a query with no words, `Failed` when the board cannot
    /// be read.
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

    /// Card `id` in full, the links other cards hold to it included.
    ///
    /// # Errors
    ///
    /// `Invalid` for an id that is not a card name, `NotFound` for no such
    /// card, `Failed` when the board cannot be read.
    async fn get(&self, id: &str) -> Result<Issue, TrackerError> {
        let id = check_id(id).map_err(TrackerError::Invalid)?;
        let front = self.read(id).await?;
        let mut issue = self.issue(id, &front);
        issue
            .links
            .extend(inverse_links(id, &front, &self.all().await?));
        Ok(issue)
    }

    /// A new card, its id made from its title.
    ///
    /// # Errors
    ///
    /// `Invalid` for no title or a status or a priority the board does not
    /// have, `Failed` when the card cannot be written.
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

    /// Changes card `id` and returns it as it now is.
    ///
    /// # Errors
    ///
    /// `Invalid` for an empty patch, an empty title or a value the board
    /// does not take, `NotFound` for no such card, `Failed` when it cannot be
    /// read or written.
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

    /// Removes card `id`'s file.
    ///
    /// # Errors
    ///
    /// `Invalid` for an id that is not a card name, `NotFound` for no such
    /// card, `Failed` when the file cannot be removed.
    async fn delete(&self, id: &str) -> Result<(), TrackerError> {
        let id = check_id(id).map_err(TrackerError::Invalid)?;
        let _one_writer = self.writes.lock().await;
        let path = self.path(id);
        if !self.fs.exists(&path).await {
            return Err(TrackerError::NotFound(format!("there is no card \"{id}\"")));
        }
        (self.fs.remove(&path).await).map_err(|e| self.failed("delete", &path, e))
    }

    /// Adds or removes `link` on card `id`.
    ///
    /// # Errors
    ///
    /// `Invalid` for an id that is not a card name or a link to itself,
    /// `NotFound` for no such card, `Failed` when it cannot be read or
    /// written.
    async fn link(&self, id: &str, link: &Link, change: LinkChange) -> Result<(), TrackerError> {
        self.change_link(id, link, change).await
    }
}
