//! PARITY: every event has its `HookSet` binding, and its `Mod` event on
//! `HookSet` runs what is bound there.

use std::sync::Mutex;

use async_trait::async_trait;

use super::*;

/// A hook of any event that notes its event's name and passes.
#[derive(Clone, Default)]
struct Names(Arc<Mutex<Vec<&'static str>>>);

#[async_trait]
impl<E: Event> Hook<E> for Names {
    async fn run(&self, _: E, _: &CancellationToken) -> Result<E::Decision, BoxError> {
        self.0.lock().unwrap().push(E::NAME.as_str());
        Ok(E::Decision::default())
    }
}

/// A new event fails to compile until it is bound through its own `on_*`
/// method (which its row in events.rs writes; `testing::bind` matches on
/// every event) and fired through its `Mod` method (`fire` does);
/// `impl Mod for HookSet` names every method, so a `Mod` event `HookSet`
/// does not fire fails the gates there.
#[tokio::test]
async fn every_event_has_a_binding_and_its_mod_event_runs_it() {
    let log = Names::default();
    let hooks = ModEvent::ALL.iter().fold(HookSet::new(), |hooks, event| {
        bind(hooks, *event, log.clone())
    });
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
    for event in ModEvent::ALL {
        fire(&hooks, *event, &haest).await.unwrap();
    }
    assert_eq!(*log.0.lock().unwrap(), ModEvent::WORDS);
}
