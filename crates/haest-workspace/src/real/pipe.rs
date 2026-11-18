//! Reading a child's output pipes to their end, so it never blocks on a full
//! one: each stream's first [`MAX_OUTPUT_BYTES`] kept, or every chunk handed
//! to the caller's `on_output` as whole characters.

use std::{
    any::Any,
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Mutex, MutexGuard},
};

use haest_core::workspace::OutputChunks;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::MAX_OUTPUT_BYTES;

/// What `catch_unwind` hands over: the payload a panic carries.
pub(super) type Panic = Box<dyn Any + Send>;

/// One stream's kept bytes.
pub(super) type Sink = Arc<Mutex<Vec<u8>>>;

/// A stream's kept bytes. The lock is held only to copy bytes in or out, so a
/// poisoned one is a bug here.
fn kept(s: &Sink) -> MutexGuard<'_, Vec<u8>> {
    s.lock().expect("the kept output's lock poisoned")
}

/// Everything `s` kept, decoded; it keeps nothing after.
pub(super) fn take(s: &Sink) -> String {
    String::from_utf8_lossy(&std::mem::take(&mut *kept(s))).into_owned()
}

/// The caller's `on_output`, shared by a call's two readers. It runs on a
/// reader's task, so its panic would end that reader and close the pipe
/// under the child: the panic is kept instead, for the call to raise once it
/// has cleaned up, and `on_output` hears nothing more.
pub(super) struct Listener {
    on_output: OutputChunks,
    panicked: Mutex<Option<Panic>>,
}

impl Listener {
    pub(super) fn new(on_output: OutputChunks) -> Self {
        Self {
            on_output,
            panicked: Mutex::default(),
        }
    }

    fn hear(&self, text: &str) {
        if self.panicked().is_some() {
            return;
        }
        if let Err(panic) = panic::catch_unwind(AssertUnwindSafe(|| (self.on_output)(text))) {
            self.panicked().get_or_insert(panic);
        }
    }

    /// The first panic of `on_output`, if it panicked.
    pub(super) fn take_panic(&self) -> Option<Panic> {
        self.panicked().take()
    }

    /// Held only to read or set the panic, never while `on_output` runs, so
    /// a poisoned one is a bug here.
    fn panicked(&self) -> MutexGuard<'_, Option<Panic>> {
        self.panicked
            .lock()
            .expect("the output listener's panic lock poisoned")
    }
}

/// How many bytes at the end of `b` begin a character the next chunk completes.
fn cut_char(b: &[u8]) -> usize {
    for back in 1..=b.len().min(3) {
        let byte = b[b.len() - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let need = match byte {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        return if need > back { back } else { 0 };
    }
    0
}

/// The text at the front of `pending` that is whole characters (all of it at
/// the end of the stream); bytes that are not UTF-8 become U+FFFD.
fn whole_chars(pending: &mut Vec<u8>, end: bool) -> String {
    let upto = if end {
        pending.len()
    } else {
        pending.len() - cut_char(pending)
    };
    let text = String::from_utf8_lossy(&pending[..upto]).into_owned();
    pending.drain(..upto);
    text
}

/// Read `from` to its end, so the child never blocks on a full pipe: into
/// `into`, its first [`MAX_OUTPUT_BYTES`] kept and the rest dropped, or to
/// `stream` as whole characters.
pub(super) async fn drain(
    mut from: impl AsyncRead + Unpin,
    into: Sink,
    stream: Option<Arc<Listener>>,
) {
    let mut chunk = vec![0u8; 16 * 1024];
    let mut pending = Vec::new();
    loop {
        let n = match from.read(&mut chunk).await {
            Ok(0) | Err(_) => 0,
            Ok(n) => n,
        };
        let Some(listener) = &stream else {
            if n == 0 {
                return;
            }
            let mut kept = kept(&into);
            let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
            kept.extend_from_slice(&chunk[..n.min(room)]);
            continue;
        };
        pending.extend_from_slice(&chunk[..n]);
        let text = whole_chars(&mut pending, n == 0);
        if !text.is_empty() {
            listener.hear(&text);
        }
        if n == 0 {
            return;
        }
    }
}
