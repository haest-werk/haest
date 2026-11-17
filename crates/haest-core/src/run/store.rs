//! Where durable runs are kept: the [`RunStore`] trait, and the memory store.

use std::{
    collections::HashMap,
    fmt, io,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;

use super::{ReplayedRun, RunKey, replay_run};
use crate::session::{SessionRecord, SessionSink, SessionSinkRef};

/// PLUG POINT: where durable runs keep their records, one log per
/// [`RunKey`]. Set it with [`SharedDeps::with_run_store`](crate::SharedDeps::with_run_store);
/// with none, nothing is durable.
///
/// [`MemoryRunStore`] keeps the logs in memory. A host that keeps its own
/// files (or a database, or a remote service) implements this: `sink` is
/// where a run's records go, `load` gives them back. Keep each record as one
/// line of its JSON (`serde_json`) and `load` is [`replay_run`] on the text.
///
/// # Examples
///
/// A store over the host's own map of logs:
///
/// ```
/// use std::{collections::HashMap, io, sync::{Arc, Mutex}};
///
/// use async_trait::async_trait;
/// use haest_core::{
///     run::{ReplayedRun, RunKey, RunStore, replay_run},
///     session::{SessionRecord, SessionSink, SessionSinkRef},
/// };
///
/// #[derive(Default)]
/// struct Logs(Arc<Mutex<HashMap<RunKey, String>>>);
///
/// struct Log(Arc<Mutex<HashMap<RunKey, String>>>, RunKey);
///
/// impl SessionSink for Log {
///     fn append(&self, record: &SessionRecord) {
///         let line = serde_json::to_string(record).expect("a record always serializes");
///         let mut logs = self.0.lock().expect("not poisoned");
///         let log = logs.entry(self.1.clone()).or_default();
///         log.push_str(&line);
///         log.push('\n');
///     }
/// }
///
/// #[async_trait]
/// impl RunStore for Logs {
///     fn sink(&self, key: &RunKey) -> SessionSinkRef {
///         Log(self.0.clone(), key.clone()).into()
///     }
///     async fn load(&self, key: &RunKey) -> io::Result<Option<ReplayedRun>> {
///         let logs = self.0.lock().map_err(|_| io::Error::other("a writer panicked"))?;
///         Ok(logs.get(key).and_then(|text| replay_run(text)))
///     }
/// }
/// ```
#[async_trait]
pub trait RunStore: Send + Sync {
    /// Where the run `key`'s records go. Records land in the order appended;
    /// [`SessionSink::flush`] resolves once they all have.
    fn sink(&self, key: &RunKey) -> SessionSinkRef;

    /// The run `key`'s records so far, rebuilt; None for a key never started.
    ///
    /// # Errors
    ///
    /// The store's own: it could not read the records.
    async fn load(&self, key: &RunKey) -> io::Result<Option<ReplayedRun>>;
}

crate::plug_ref! {
    /// A [`RunStore`], shared: what [`SharedDeps::run_store`](crate::SharedDeps::run_store) holds.
    pub struct RunStoreRef(dyn RunStore);
}

type Logs = Arc<Mutex<HashMap<RunKey, Vec<String>>>>;

/// The logs. Only this file's code runs under the lock, so a poisoned one
/// is a bug here.
fn lock(logs: &Logs) -> MutexGuard<'_, HashMap<RunKey, Vec<String>>> {
    logs.lock().expect("the memory run store's lock poisoned")
}

/// Durable runs kept in memory, as JSON lines: they survive an aborted or
/// dropped turn, not the process. For tests, and for a host that only needs
/// a run to survive a turn it stopped. haest deletes nothing from it.
///
/// Cheap to clone: the clones share the same logs.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::run::RunError> {
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, ShutWorkspace, Tools,
///     run::{MemoryRunStore, RunKey, Served},
///     testing::{ScriptedProvider, text_answer},
/// };
///
/// let store = MemoryRunStore::new();
/// let provider = ScriptedProvider::new([text_answer("done")]);
/// let deps = AgentDeps::new(provider.resolver(), Tools::new(), "", "/", ShutWorkspace)
///     .with_run_store(store.clone());
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
/// let key = RunKey::new("job-1");
///
/// let first = agent.run_durable(&key, "do it", &mut Vec::<LoopEvent>::new()).await?;
/// assert_eq!((first.served, first.text.as_str()), (Served::Fresh, "done"));
/// // The run's records: run, the input, the answer, the turn state, run_end.
/// assert!(store.lines(&key).len() >= 4);
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
#[derive(Clone, Default)]
pub struct MemoryRunStore {
    logs: Logs,
}

/// The keys it holds records for, sorted; not the records.
impl fmt::Debug for MemoryRunStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryRunStore")
            .field("keys", &self.keys())
            .finish_non_exhaustive()
    }
}

impl MemoryRunStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every record kept under `key`, oldest first, one JSON record each:
    /// the lines a session file of the run would hold. Empty for a key never
    /// started.
    pub fn lines(&self, key: &RunKey) -> Vec<String> {
        lock(&self.logs).get(key).cloned().unwrap_or_default()
    }

    /// Every key with records, sorted.
    pub fn keys(&self) -> Vec<RunKey> {
        let mut keys: Vec<RunKey> = lock(&self.logs).keys().cloned().collect();
        keys.sort();
        keys
    }
}

#[async_trait]
impl RunStore for MemoryRunStore {
    fn sink(&self, key: &RunKey) -> SessionSinkRef {
        MemorySink {
            logs: self.logs.clone(),
            key: key.clone(),
        }
        .into()
    }

    async fn load(&self, key: &RunKey) -> io::Result<Option<ReplayedRun>> {
        Ok(replay_run(&self.lines(key).join("\n")))
    }
}

/// One run's log in a [`MemoryRunStore`]: every record lands as it is appended.
struct MemorySink {
    logs: Logs,
    key: RunKey,
}

impl SessionSink for MemorySink {
    fn append(&self, record: &SessionRecord) {
        let line = serde_json::to_string(record).expect("a session record always serializes");
        let mut logs = lock(&self.logs);
        logs.entry(self.key.clone()).or_default().push(line);
    }
}
