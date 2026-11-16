//! What crosses between haest and an engine thread ([`super::run_in_vm`] <->
//! [`super::host`]), and a run's result.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
};

use serde_json::{Map, Value};

/// The bounds of one engine run, for [`run_in_vm`](super::run_in_vm).
///
/// The default is the Code tool's when the model sets no timeout: a minute,
/// [`VM_MEMORY_BYTES`](super::VM_MEMORY_BYTES), 100,000 characters of output.
///
/// ```
/// use haest_code::VmCaps;
///
/// let quick = VmCaps { timeout_ms: 5_000, ..VmCaps::default() };
/// assert_eq!(quick.memory_bytes, haest_code::vm::VM_MEMORY_BYTES);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmCaps {
    /// Wall-clock deadline in milliseconds, from when the engine starts; past
    /// it the run is stopped as [`VmStop::Timeout`], even mid-loop.
    pub timeout_ms: u64,
    /// QuickJS heap limit in bytes; an allocation past it is an error in the guest.
    pub memory_bytes: usize,
    /// Console output kept, in characters; the rest is dropped and
    /// [`VmRun::logs_truncated`] set.
    pub max_log_chars: usize,
}

impl Default for VmCaps {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,
            memory_bytes: super::VM_MEMORY_BYTES,
            max_log_chars: 100_000,
        }
    }
}

/// Why a run was cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmStop {
    /// It reached [`VmCaps::timeout_ms`].
    Timeout,
    /// The caller's cancel token fired.
    Interrupted,
}

/// How one engine run ended: its value or error, what it printed, and whether
/// it was stopped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VmRun {
    /// Function mode: the code's return value -- a string as is, anything
    /// else as JSON (indented two spaces), with the promises inside a plain
    /// object or array (a few levels deep) awaited first. None for
    /// `undefined`, in module mode, or when the run failed.
    pub value: Option<String>,
    /// The uncaught error, `Name: message (line N)` with the line in the
    /// code's own numbering; or why the engine could not run it.
    pub error: Option<String>,
    /// Interleaved, non-log levels tagged (`[warn] ...`): what the Code tool shows the model.
    pub logs: String,
    /// `console.log`, `info` and `debug` lines.
    pub stdout: String,
    /// `console.warn` and `error` lines.
    pub stderr: String,
    /// Console output passed [`VmCaps::max_log_chars`] and the rest was dropped.
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
    /// Time since the engine started, in milliseconds.
    pub elapsed_ms: u64,
    /// Host function calls the guest has made so far.
    pub calls: u64,
    /// Of those, the ones not answered yet.
    pub inflight: u64,
    /// The engine's heap use, as last measured; it is measured only while the
    /// run waits on haest, so a CPU loop's beats repeat the last figure.
    pub memory_bytes: u64,
}

/// Everything a run needs, as plain data: closures stay on haest's side.
#[derive(Debug, Clone)]
pub(crate) struct VmJob {
    pub code: String,
    pub prelude: String,
    /// JSON globals set before the prelude runs.
    pub values: Map<String, Value>,
    /// Names of the guest functions answered by haest (async: a promise in the guest).
    pub async_names: Vec<String>,
    /// ES module mode: importable name -> source. None = function mode (`return` works).
    pub modules: Option<HashMap<String, String>>,
    pub filename: String,
    pub caps: VmCaps,
    /// haest sets it to stop the run; the interrupt handler reads it, so it reaches
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
