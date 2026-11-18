//! [`TrackerRef`], written out: haest-issues is versioned on its own, so it
//! uses only haest-core's public items, and haest's own handles come from a
//! hidden macro. This is what that macro makes for a plug point.

use std::{fmt, ops::Deref, sync::Arc};

use super::Tracker;

/// A [`Tracker`], shared: what the issue tools hold. Built from the
/// tracker itself (a [`CardTracker`](crate::CardTracker), a
/// [`GithubTracker`](crate::GithubTracker), a host's own), or from an
/// `Arc` of one, so [`issue_tools`](crate::issue_tools) takes either.
#[derive(Clone)]
pub struct TrackerRef(Arc<dyn Tracker>);

impl TrackerRef {
    /// A handle to `value`. [`From`] does the same, so a parameter that takes
    /// `impl Into<TrackerRef>` takes the value itself.
    pub fn new(value: impl Tracker + 'static) -> Self {
        Self(Arc::new(value))
    }

    /// Whether both handles hold the same object (one is a clone of the other).
    #[must_use]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        Arc::ptr_eq(&this.0, &other.0)
    }
}

impl<T: Tracker + 'static> From<T> for TrackerRef {
    fn from(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T: Tracker + 'static> From<Arc<T>> for TrackerRef {
    fn from(value: Arc<T>) -> Self {
        Self(value)
    }
}

impl From<Arc<dyn Tracker>> for TrackerRef {
    fn from(value: Arc<dyn Tracker>) -> Self {
        Self(value)
    }
}

impl From<Box<dyn Tracker>> for TrackerRef {
    fn from(value: Box<dyn Tracker>) -> Self {
        Self(value.into())
    }
}

impl AsRef<dyn Tracker> for TrackerRef {
    fn as_ref(&self) -> &(dyn Tracker + 'static) {
        &*self.0
    }
}

impl Deref for TrackerRef {
    type Target = dyn Tracker;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl fmt::Debug for TrackerRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TrackerRef")
            .field(&self.0.info().name)
            .finish()
    }
}
