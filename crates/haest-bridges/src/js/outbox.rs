//! What a file mod sends out, drained from its runtime after every request:
//! registrations, notices, asks, and the results of the calls that finished.
//! Registrations land before any reply, so a loaded mod's tools are there by
//! the time `load` returns.

use haest_mods::{ApprovalDecision, ApprovalRequest};
use serde::Deserialize;

use super::{
    channel::{JsMeta, ToJs, ToolChange, ToolMeta},
    thread::Js,
};

#[derive(Deserialize)]
struct Drained {
    /// `(call id, ok, JSON result or error text)`.
    settled: Vec<(u64, bool, String)>,
    outbox: Vec<Out>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Out {
    On {
        event: String,
    },
    Tool {
        tool: ToolMeta,
    },
    RemoveTool {
        name: String,
    },
    DisableTool {
        name: String,
    },
    EnableTool {
        name: String,
    },
    Command {
        name: String,
    },
    Ask {
        id: u64,
        req: serde_json::Value,
        signal: Option<u64>,
    },
    Notice {
        text: String,
    },
}

impl Js {
    pub(super) fn apply(&mut self, json: &str) {
        let drained: Drained = match serde_json::from_str(json) {
            Ok(d) => d,
            Err(e) => {
                return self.fail_all(&format!("its runtime sent something unreadable: {e}"));
            }
        };
        for out in drained.outbox {
            self.forward(out);
        }
        for (id, ok, text) in drained.settled {
            self.finish(id, if ok { Ok(text) } else { Err(text) });
        }
    }

    fn forward(&mut self, out: Out) {
        match out {
            Out::On { event } => {
                *JsMeta::lock(&self.meta).handlers.entry(event).or_default() += 1;
            }
            Out::Tool { tool } => {
                let change = ToolChange::Set(tool);
                JsMeta::lock(&self.meta).tool_changes.push(change);
            }
            Out::RemoveTool { name } => {
                let change = ToolChange::Remove(name);
                JsMeta::lock(&self.meta).tool_changes.push(change);
            }
            Out::DisableTool { name } => JsMeta::lock(&self.meta).set_disabled(name, true),
            Out::EnableTool { name } => JsMeta::lock(&self.meta).set_disabled(name, false),
            Out::Command { name } => JsMeta::lock(&self.meta).add_command(name),
            Out::Notice { text } => self.ui.info(&text),
            Out::Ask { id, req, signal } => self.ask(id, req, signal),
        }
    }

    /// Put the mod's ask to the host on the async runtime; the answer comes back
    /// as a request. Under `ctx.signal` it ends when that call is interrupted.
    fn ask(&self, id: u64, req: serde_json::Value, signal: Option<u64>) {
        let req: ApprovalRequest = match serde_json::from_value(req) {
            Ok(r) => r,
            Err(e) => {
                let result = Err(format!("haest.ask(req): {e}"));
                let _ = self.own.send(ToJs::Answer { id, result });
                return;
            }
        };
        let cancel = signal
            .and_then(|s| self.signals.get(&s).cloned())
            .unwrap_or_default();
        let (ui, own) = (self.ui.clone(), self.own.clone());
        self.runtime.spawn(async move {
            let decision = if cancel.is_cancelled() {
                ApprovalDecision::Deny
            } else {
                ui.ask(req, &cancel).await
            };
            let result = Ok(decision.as_str());
            let _ = own.send(ToJs::Answer { id, result });
        });
    }

    pub(super) fn finish(&mut self, id: u64, result: Result<String, String>) {
        self.signals.remove(&id);
        if let Some(reply) = self.pending.remove(&id) {
            let _ = reply.send(result);
        }
    }

    pub(super) fn fail_all(&mut self, why: &str) {
        let ids: Vec<u64> = self.pending.keys().copied().collect();
        for id in ids {
            self.finish(id, Err(why.to_string()));
        }
    }
}
