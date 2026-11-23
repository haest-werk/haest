//! What crosses between haest and an engine thread ([`super::run_in_vm`] <->
//! [`super::thread`]), and a run's result.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use serde_json::{Map, Value};

/// The bounds of one engine: a run of [`run_in_vm`](super::run_in_vm) (the
/// Code tool's, the sandbox `js` command's) or a mod file (haest-bridges'
/// `JsMod`). A mod file's caps start from [`VmCaps::mod_file`]: 128 MiB,
/// 30 s to load, 10 s of code without a pause. [`VmCaps::default`] is the
/// Code tool's when the model sets no timeout: a minute,
/// [`VM_MEMORY_BYTES`](super::VM_MEMORY_BYTES) (64 MiB), 100,000 characters
/// of output. A mod file given caps built on `default()` gets the Code
/// tool's memory and timeout, not its own.
///
/// All of haest's JavaScript runs on the same engine, held to this one type;
/// each field says which runs read it.
///
/// ```
/// use std::time::Duration;
///
/// use haest_code::VmCaps;
///
/// let quick = VmCaps::default().with_timeout(Duration::from_secs(5));
/// assert_eq!(quick.memory_bytes, haest_code::VM_MEMORY_BYTES);
///
/// let strict = VmCaps::mod_file().with_timeout(Duration::from_secs(5));
/// assert_eq!(strict.memory_bytes, 128 << 20);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct VmCaps {
    /// QuickJS heap limit in bytes; an allocation past it is an error in the
    /// guest. Every run reads it.
    pub memory_bytes: usize,
    /// How long a run has, wall clock, waiting included. A run of
    /// [`run_in_vm`](super::run_in_vm) is stopped past it, from when its
    /// engine starts, even mid-loop ([`VmStop::Timeout`]). A mod file's
    /// default export must return within it when the mod loads; after that
    /// the mod's calls have no deadline, and [`slice`](Self::slice) bounds
    /// its code. Thirty years or more (`Duration::MAX`) is no deadline.
    pub timeout: Duration,
    /// The longest a mod file's code may run without a pause (awaiting does
    /// not count). Past it the code is stopped: every call into the mod in
    /// flight fails, and the next call runs as usual. A run of
    /// [`run_in_vm`](super::run_in_vm) does not read it: its
    /// [`timeout`](Self::timeout) stops a CPU loop.
    pub slice: Duration,
    /// Console output kept, in characters; the rest is dropped and
    /// [`VmRun::logs_truncated`] set. A mod file has no console and does not
    /// read it.
    pub max_log_chars: usize,
}

impl Default for VmCaps {
    fn default() -> Self {
        Self {
            memory_bytes: super::VM_MEMORY_BYTES,
            timeout: Duration::from_secs(60),
            slice: Duration::from_secs(10),
            max_log_chars: 100_000,
        }
    }
}

impl VmCaps {
    /// A mod file's caps: 128 MiB of memory, 30 s for its default export to
    /// return, 10 s of code without a pause.
    pub fn mod_file() -> Self {
        Self {
            memory_bytes: 128 << 20,
            timeout: Duration::from_secs(30),
            ..Self::default()
        }
    }

    /// The QuickJS heap limit, in bytes.
    #[must_use]
    pub fn with_memory_bytes(mut self, bytes: usize) -> Self {
        self.memory_bytes = bytes;
        self
    }

    /// How long a run has, wall clock, waiting included; for a mod file, how
    /// long its default export has to return.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The longest a mod file's code may run without a pause.
    #[must_use]
    pub fn with_slice(mut self, slice: Duration) -> Self {
        self.slice = slice;
        self
    }

    /// Console output kept, in characters.
    #[must_use]
    pub fn with_max_log_chars(mut self, chars: usize) -> Self {
        self.max_log_chars = chars;
        self
    }
}

/// Why a run was cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VmStop {
    /// It reached [`VmCaps::timeout`].
    Timeout,
    /// The caller's cancel token fired.
    Interrupted,
}

/// How one engine run ended: its value or error, what it printed, and whether
/// it was stopped.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
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
