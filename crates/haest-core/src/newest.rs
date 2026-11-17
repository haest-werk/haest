//! [`Newest`]: the newest value of something that changes, handed to a
//! host's hook one call at a time and never under the owner's lock.

use std::{
    fmt,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

/// The newest value of something that changes (a task list, a ledger's
/// snapshot, the jobs or subagents running), handed to a host's hook in the
/// order of the changes, one call at a time, with no lock held: the hook may
/// read (and change) its owner, and a hook that panics poisons nothing.
///
/// The owner [`put`](Self::put)s each new value while it holds its own lock,
/// so the values arrive in the order of the changes, then calls
/// [`deliver`](Self::deliver) with that lock released. A caller that finds a
/// delivery running leaves its value to that one and returns at once; values
/// that land while the hook runs are handed over as one call with the
/// newest. So the hook never hears an older value after a newer one, and the
/// last value it hears is the owner's state.
///
/// haest's ledger, task board, job hub and subagent hub hand their hooks
/// their state this way; a plug-in with a hook of its own uses it too.
///
/// # Examples
///
/// ```
/// use std::sync::Mutex;
///
/// use haest_core::newest::Newest;
///
/// /// Names a host shows, and every change to them.
/// #[derive(Default)]
/// struct Roster {
///     names: Mutex<Vec<String>>,
///     changes: Newest<Vec<String>>,
/// }
///
/// impl Roster {
///     fn add(&self, name: &str, on_change: impl Fn(&Vec<String>)) {
///         {
///             let mut names = self.names.lock().expect("the roster's lock poisoned");
///             names.push(name.to_string());
///             // At the change, under the owner's lock: in the order of the changes.
///             self.changes.put(names.clone());
///         }
///         // The lock released: the hook may read the roster.
///         self.changes.deliver(on_change);
///     }
/// }
///
/// let roster = Roster::default();
/// let heard = Mutex::new(Vec::new());
/// let hook = |names: &Vec<String>| heard.lock().expect("poisoned").push(names.len());
/// roster.add("scout", hook);
/// roster.add("builder", hook);
/// assert_eq!(*heard.lock().expect("poisoned"), [1, 2]);
/// ```
pub struct Newest<T> {
    /// The value not handed over yet, if any.
    slot: Mutex<Option<T>>,
    /// A caller is handing values over.
    delivering: AtomicBool,
}

impl<T> Default for Newest<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a value waits, and whether a delivery runs; never the value.
impl<T> fmt::Debug for Newest<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let waiting = self.slot.try_lock().map(|slot| slot.is_some());
        let mut out = f.debug_struct("Newest");
        match waiting {
            Ok(waiting) => out.field("waiting", &waiting),
            Err(_) => out.field("waiting", &format_args!("<locked>")),
        };
        out.field("delivering", &self.delivering.load(Ordering::Relaxed))
            .finish()
    }
}

impl<T> Newest<T> {
    /// Nothing put yet.
    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            delivering: AtomicBool::new(false),
        }
    }

    /// `value` is the newest: call it under the owner's lock, at the change.
    pub fn put(&self, value: T) {
        *self.slot() = Some(value);
    }

    /// Hand `hook` every value put and not handed over yet, newest last,
    /// unless another caller is doing so: then that one hands this caller's
    /// value over too, with its own hook. So every caller passes the same
    /// hook (the owner's). Call it with the owner's lock released.
    ///
    /// # Panics
    ///
    /// When `hook` panics: the panic goes on to this caller, and the next
    /// delivery works.
    pub fn deliver(&self, hook: impl Fn(&T)) {
        // A value put while the last delivery was ending is seen here: either
        // this check finds it, or its caller found `delivering` cleared.
        while self.has_value() && self.claim() {
            let _done = Done(&self.delivering);
            loop {
                // The guard drops with this statement: the hook runs unlocked.
                let Some(value) = self.slot().take() else {
                    break;
                };
                hook(&value);
            }
        }
    }

    fn has_value(&self) -> bool {
        self.slot().is_some()
    }

    /// Become the one caller delivering; false when another one is.
    fn claim(&self) -> bool {
        self.delivering
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// The slot. Its lock is held only to swap the value, never while a hook
    /// runs, so a poisoned one is a bug here.
    fn slot(&self) -> MutexGuard<'_, Option<T>> {
        self.slot.lock().expect("the newest value's lock poisoned")
    }
}

/// Clears the delivering flag when the delivery ends, a hook's panic included.
struct Done<'a>(&'a AtomicBool);

impl Drop for Done<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Barrier},
    };

    use super::*;

    #[test]
    fn each_value_is_handed_over_in_order_when_no_one_else_delivers() {
        let newest = Newest::default();
        let heard = Mutex::new(Vec::new());
        for v in 1..=3 {
            newest.put(v);
            newest.deliver(|v| heard.lock().unwrap().push(*v));
        }
        assert_eq!(*heard.lock().unwrap(), [1, 2, 3]);
    }

    /// A value put while another caller's hook runs is handed over by that
    /// caller, after the value it was handing over, and only the newest.
    #[test]
    fn values_put_during_a_delivery_are_handed_over_by_it_newest_only() {
        let newest = Arc::new(Newest::default());
        let (in_hook, go_on) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
        let heard = Arc::new(Mutex::new(Vec::new()));
        let first = std::thread::spawn({
            let (newest, in_hook, go_on, heard) = (
                newest.clone(),
                in_hook.clone(),
                go_on.clone(),
                heard.clone(),
            );
            move || {
                newest.put(1);
                newest.deliver(|v| {
                    if *v == 1 {
                        in_hook.wait();
                        go_on.wait();
                    }
                    heard.lock().unwrap().push(*v);
                });
            }
        });
        in_hook.wait();
        for v in [2, 3] {
            newest.put(v);
            // Returns at once: the first caller is delivering.
            newest.deliver(|v| heard.lock().unwrap().push(*v + 100));
        }
        go_on.wait();
        first.join().unwrap();
        assert_eq!(*heard.lock().unwrap(), [1, 3]);
    }

    #[test]
    fn a_hook_that_panics_leaves_the_next_delivery_working() {
        let newest = Newest::default();
        newest.put(1);
        let panicked = catch_unwind(AssertUnwindSafe(|| newest.deliver(|_| panic!("host bug"))));
        assert!(panicked.is_err());
        let heard = Mutex::new(Vec::new());
        newest.put(2);
        newest.deliver(|v| heard.lock().unwrap().push(*v));
        assert_eq!(*heard.lock().unwrap(), [2]);
    }
}
