//! What crosses between kern and an engine thread ([`super::run_in_vm`] <->
//! [`super::host`]), and a run's result.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
};

use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmLimits {
    pub timeout_ms: u64,
    pub memory_bytes: usize,
    pub max_log_chars: usize,
}

/// Why a run was cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmStop {
    Timeout,
    Interrupted,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VmRun {
    /// Function mode: the code's return value -- a string as is, anything else as JSON.
    pub value: Option<String>,
    /// `Name: message (line N)`.
    pub error: Option<String>,
    /// Interleaved, non-log levels tagged (`[warn] ...`): what the Code tool shows the model.
    pub logs: String,
    pub stdout: String,
    pub stderr: String,
    pub logs_truncated: bool,
    /// Why the run was cut short, or None.
    pub stopped: Option<VmStop>,
    /// The engine did not stop when asked and was abandoned: whatever it printed is lost.
    pub killed: bool,
}

impl VmRun {
    pub(crate) fn failed(error: impl Into<String>) -> Self {
        Self {
            error: Some(error.into()),
            ..Default::default()
        }
    }

    pub(crate) fn stopped(why: VmStop) -> Self {
        Self {
            stopped: Some(why),
            ..Default::default()
        }
    }
}

/// One heartbeat: how the run is doing, about once a second -- from the interrupt
/// handler, so a CPU loop beats too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmBeat {
    pub elapsed_ms: u64,
    pub calls: u64,
    pub inflight: u64,
    pub memory_bytes: u64,
}

/// Everything a run needs, as plain data: closures stay on kern's side.
#[derive(Debug, Clone)]
pub(crate) struct VmJob {
    pub code: String,
    pub prelude: String,
    /// JSON globals set before the prelude runs.
    pub values: Map<String, Value>,
    /// Names of the guest functions answered by kern (async: a promise in the guest).
    pub async_names: Vec<String>,
    /// ES module mode: importable name -> source. None = function mode (`return` works).
    pub modules: Option<HashMap<String, String>>,
    pub filename: String,
    pub limits: VmLimits,
    /// kern sets it to stop the run; the interrupt handler reads it, so it reaches
    /// a CPU loop where no message could.
    pub stop: Arc<AtomicBool>,
}

#[derive(Debug)]
pub(crate) enum ToVm {
    Answer {
        call_id: u64,
        ok: bool,
        text: String,
    },
    Stop,
}

#[derive(Debug)]
pub(crate) enum FromVm {
    Call {
        call_id: u64,
        name: String,
        args: Vec<String>,
    },
    Beat(VmBeat),
    Done(VmRun),
}
