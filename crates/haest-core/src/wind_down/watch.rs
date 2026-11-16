//! [`WindDownWatch`]: what a tool asks.

use std::{fmt, sync::Arc};

use super::WindingDown;

type Check = dyn Fn() -> Option<WindingDown> + Send + Sync;

/// A running turn's wind-down, as its tools see it
/// ([`ToolContext::wind_down`](crate::ToolContext::wind_down)): is the run
/// past its share of a cap right now? `Default` never is.
///
/// The loop hands every call one, asked live: what earlier calls of the same
/// answer spent counts (a subagent that just finished), and so does the
/// conversation's ledger as other agents charge it.
///
/// ```
/// use haest_core::wind_down::{WindDownWatch, WindingDown};
///
/// // Outside the loop (a test, a host command): a watch of the host's own.
/// let fact = "the batch is nearly done".to_string();
/// let past = WindDownWatch::new(move || Some(WindingDown { fact: fact.clone(), limit: None, remedy: None }));
/// assert!(past.check().is_some());
/// assert!(WindDownWatch::default().check().is_none());
/// ```
#[derive(Clone, Default)]
pub struct WindDownWatch(Option<Arc<Check>>);

impl WindDownWatch {
    /// A watch that `check` answers.
    pub fn new(check: impl Fn() -> Option<WindingDown> + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(check)))
    }

    /// The cap the run is past its share of now, or None.
    pub fn check(&self) -> Option<WindingDown> {
        self.0.as_ref().and_then(|c| c())
    }
}

impl fmt::Debug for WindDownWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            Some(_) => "WindDownWatch(on)",
            None => "WindDownWatch(off)",
        })
    }
}
