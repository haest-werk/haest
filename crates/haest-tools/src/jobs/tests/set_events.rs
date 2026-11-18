//! A listener the hub lets go of is dropped with no lock of the hub's held:
//! its drop is a host's code, and may call the hub back.

use std::{sync::mpsc, time::Duration};

use tokio::sync::mpsc::unbounded_channel;

use crate::jobs::{JobEvents, JobHub, NoJobEvents};

/// A listener whose drop hands the hub a listener of its own (a host
/// re-registering as it goes).
struct CallsBackOnDrop(JobHub);

impl JobEvents for CallsBackOnDrop {}

impl Drop for CallsBackOnDrop {
    fn drop(&mut self) {
        self.0.set_events(NoJobEvents);
    }
}

/// The hub held the last handle to the old listener, and dropped it while
/// it held the listener lock: the drop's `set_events` waited for that lock
/// for good.
#[test]
fn a_listener_set_events_lets_go_of_may_call_the_hub_back() {
    let (inbox, _notices) = unbounded_channel();
    let hub = JobHub::new(inbox);
    hub.set_events(CallsBackOnDrop(hub.clone()));
    let (done, finished) = mpsc::channel();
    let replacing = hub.clone();
    std::thread::spawn(move || {
        replacing.set_events(NoJobEvents);
        let _ = done.send(());
    });
    let returned = finished.recv_timeout(Duration::from_secs(5));
    assert!(
        returned.is_ok(),
        "set_events never returned: the old listener's drop ran under the hub's lock"
    );
}
