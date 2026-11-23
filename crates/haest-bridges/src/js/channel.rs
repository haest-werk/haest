//! The async side of a mod's thread: the requests it takes, the handle that
//! sends them, and what the mod has registered so far.

use std::{
    collections::HashSet,
    sync::{Mutex, MutexGuard, mpsc},
};

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(crate) type Reply = oneshot::Sender<Result<String, String>>;

pub(crate) enum ToJs {
    /// Run `op` with `payload` (JSON); `cancel` is the call's, for `haest.ask(req, ctx.signal)`.
    Call {
        op: &'static str,
        payload: String,
        cancel: Option<CancellationToken>,
        reply: Reply,
    },
    /// The host answered the mod's ask `id`.
    Answer {
        id: u64,
        result: Result<&'static str, String>,
    },
    Stop,
}

/// A tool a file mod registered, as haest advertises it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolMeta {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub kind: String,
    pub read_only: bool,
}

/// A change a file mod made to its own tools, on its way to the
/// [`LiveTools`](haest_core::LiveTools) its [`JsMod`](super::JsMod) holds.
pub(crate) enum ToolChange {
    /// `haest.tool(t)`: add it, or replace the tool of that name in place.
    Set(ToolMeta),
    /// `haest.removeTool(name)`.
    Remove(String),
}

/// What a file mod has registered so far (it may register more later).
#[derive(Default)]
pub(crate) struct JsMeta {
    pub events: HashSet<String>,
    /// Tool changes in the order the mod made them, not yet applied: the
    /// mod's thread cannot make a tool (a tool holds the thread's handle).
    pub tool_changes: Vec<ToolChange>,
    pub commands: Vec<String>,
    /// `haest.disableTool` names, once each, until `haest.enableTool`.
    pub disabled: Vec<String>,
}

impl JsMeta {
    /// `meta`, held until the guard drops. Held only to read or add a
    /// registration, never while the mod's code or the host's runs, so a
    /// poisoned one is a bug here.
    pub(super) fn lock(meta: &Mutex<Self>) -> MutexGuard<'_, Self> {
        meta.lock().expect("the mod's registrations lock poisoned")
    }

    pub(super) fn add_command(&mut self, name: String) {
        if !self.commands.contains(&name) {
            self.commands.push(name);
        }
    }

    /// Hide `name`, or show it again.
    pub(super) fn set_disabled(&mut self, name: String, disabled: bool) {
        self.disabled.retain(|n| *n != name);
        if disabled {
            self.disabled.push(name);
        }
    }
}

/// Why a call into a mod's runtime came back empty.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CallError {
    /// The mod's code threw, or answered what is not JSON: its words.
    #[error("{0}")]
    Failed(String),
    #[error("interrupted")]
    Interrupted,
    #[error("the mod is no longer running")]
    Stopped,
}

/// The way into one mod's thread. Dropping the last handle stops the thread.
pub(crate) struct JsThread {
    tx: mpsc::Sender<ToJs>,
}

impl Drop for JsThread {
    fn drop(&mut self) {
        let _ = self.tx.send(ToJs::Stop);
    }
}

impl JsThread {
    pub(crate) fn new(tx: mpsc::Sender<ToJs>) -> Self {
        Self { tx }
    }

    /// One request; `cancel` = stop waiting when it fires.
    pub(crate) async fn call(
        &self,
        op: &'static str,
        payload: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, CallError> {
        let (reply, rx) = oneshot::channel();
        let request = ToJs::Call {
            op,
            payload: payload.to_string(),
            cancel: cancel.cloned(),
            reply,
        };
        self.tx
            .send(request)
            .map_err(|_stopped| CallError::Stopped)?;
        let got = match cancel {
            Some(c) => tokio::select! {
                r = rx => r,
                _ = c.cancelled() => return Err(CallError::Interrupted),
            },
            None => rx.await,
        };
        match got {
            Ok(Ok(json)) => {
                serde_json::from_str(&json).map_err(|e| CallError::Failed(e.to_string()))
            }
            Ok(Err(message)) => Err(CallError::Failed(message)),
            Err(_) => Err(CallError::Stopped),
        }
    }
}
