//! [`Newest`]: the newest value of something that changes, handed to a
//! host's hook one call at a time and never under a lock of haest's.

use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};

/// The newest value of something that changes (a task list, a ledger's
/// snapshot), handed to a host's hook in the order of the changes, one call
/// at a time, with no lock held: the hook may read (and change) its owner,
/// and a hook that panics poisons nothing.
///
/// The owner [`put`](Self::put)s each new value while it holds its own lock,
/// so the values arrive in the order of the changes, then calls
/// [`deliver`](Self::deliver) with that lock released. A caller that finds a
/// delivery running leaves its value to that one and returns at once; values
/// that land while the hook runs are handed over as one call with the
/// newest. So the hook never hears an older value after a newer one, and the
/// last value it hears is the owner's state.
pub(crate) struct Newest<T> {
    /// The value not handed over yet, if any.
    slot: Mutex<Option<T>>,
    /// A caller is handing values over.
    delivering: AtomicBool,
}

impl<T> Default for Newest<T> {
    fn default() -> Self {
        Self {
            slot: Mutex::new(None),
            delivering: AtomicBool::new(false),
        }
    }
}

impl<T> Newest<T> {
    /// `value` is the newest: call it under the owner's lock, at the change.
    pub(crate) fn put(&self, value: T) {
        *self.slot() = Some(value);
    }

    /// Hand `hook` every value put and not handed over yet, newest last,
    /// unless another caller is doing so: then that one hands this caller's
    /// value over too. Call it with the owner's lock released.
    pub(crate) fn deliver(&self, hook: impl Fn(&T)) {
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
