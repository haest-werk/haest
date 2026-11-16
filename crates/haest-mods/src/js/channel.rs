//! The async side of a mod's thread: the requests it takes, the handle that
//! sends them, and what the mod has registered so far.

use std::{collections::HashSet, sync::mpsc};

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

/// What a file mod has registered so far (it may register more later).
#[derive(Default)]
pub(crate) struct JsMeta {
    pub events: HashSet<String>,
    pub tools: Vec<ToolMeta>,
    pub commands: Vec<String>,
}

impl JsMeta {
    /// A second tool with the same name replaces the first.
    pub fn add_tool(&mut self, tool: ToolMeta) {
        self.tools.retain(|t| t.name != tool.name);
        self.tools.push(tool);
    }
    pub fn add_command(&mut self, name: String) {
        if !self.commands.contains(&name) {
            self.commands.push(name);
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
        self.tx.send(request).map_err(|_| CallError::Stopped)?;
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
