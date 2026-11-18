//! What a setter lets go of (the host's old remedies, its old thresholds) is
//! dropped with the ledger unlocked: its drop is a host's code, and may read
//! the ledger.

use std::{sync::mpsc, thread, time::Duration};

use super::*;
use crate::threshold::{Rearm, Threshold, ThresholdRef};

/// Reads the ledger it is on when dropped.
struct ReadsOnDrop(Ledger);

impl Drop for ReadsOnDrop {
    fn drop(&mut self) {
        let _ = self.0.usd();
    }
}

/// A ledger warning at half the cap, holding a [`ReadsOnDrop`] it only
/// drops.
struct Warning {
    _held: ReadsOnDrop,
}

impl Threshold for Warning {
    fn meter(&self) -> Meter {
        Meter::Usd(Scope::Conversation)
    }
    fn mark(&self) -> Mark {
        Mark::share(0.5)
    }
    fn action(&self) -> Action {
        Action::Warn
    }
    fn rearm(&self) -> Rearm {
        Rearm::Never
    }
    fn text(&self, _: &Reading) -> String {
        "half spent".into()
    }
}

/// Whether `set` returns within 5 s, run on a thread of its own.
fn returns(set: impl FnOnce() + Send + 'static) -> bool {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        set();
        let _ = done.send(());
    });
    finished.recv_timeout(Duration::from_secs(5)).is_ok()
}

/// The ledger held the last handle to the old remedies and dropped them
/// with its lock held: their drop's read of the ledger waited for good.
#[test]
fn old_remedies_may_read_the_ledger_as_they_drop() {
    let (ledger, _) = ledger_with(10.0, 0);
    let on_drop = ReadsOnDrop(ledger.clone());
    ledger.set_remedies(Remedies::new(move |_| {
        let _held = &on_drop;
        None
    }));
    let setting = ledger.clone();
    assert!(
        returns(move || setting.set_remedies(Remedies::default())),
        "set_remedies never returned: the old remedies dropped under the ledger's lock"
    );
}

/// The same for the thresholds `set_thresholds` replaces.
#[test]
fn old_thresholds_may_read_the_ledger_as_they_drop() {
    let (ledger, _) = ledger_with(10.0, 0);
    let warning = ThresholdRef::new(Warning {
        _held: ReadsOnDrop(ledger.clone()),
    });
    ledger.set_thresholds(Thresholds::new([warning]).unwrap());
    let setting = ledger.clone();
    assert!(
        returns(move || setting.set_thresholds(Thresholds::none())),
        "set_thresholds never returned: the old thresholds dropped under the ledger's lock"
    );
}
