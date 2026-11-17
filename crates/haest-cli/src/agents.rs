//! How subagents read on the terminal: a line when one starts, one per tool
//! it ran and per guard notice, one when it stops, and the running count
//! when it changes. Its id is shown as its name.

use std::collections::HashMap;

use haest::LoopEvent;
#[cfg(test)]
use haest::tools::SubagentEvent;

use crate::render::{Level, result_line};

#[derive(Debug, Default)]
pub(crate) struct AgentLog {
    names: HashMap<String, String>,
    running: usize,
}

impl AgentLog {
    pub(crate) fn activity(&mut self, running: usize) -> Option<String> {
        if running == self.running {
            return None;
        }
        self.running = running;
        Some(format!("Subagents running: {running}"))
    }

    /// One subagent event, whichever: the lines the renderer makes from the
    /// stream's frames, in one call.
    #[cfg(test)]
    pub(crate) fn event(&mut self, id: &str, event: &SubagentEvent) -> Option<(Level, String)> {
        match event {
            SubagentEvent::Started {
                name,
                parent,
                description,
            } => Some(self.started(id, name, parent, description)),
            SubagentEvent::Loop(ev) => self.loop_event(id, ev),
            SubagentEvent::Stopped { ok } => Some(self.stopped(id, *ok)),
            // Inner calls and messages stay off the log.
            _ => None,
        }
    }

    /// The name the subagent `id` goes by: the one it started with.
    fn name(&self, id: &str) -> String {
        self.names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    /// It started: its name is kept for its lines.
    pub(crate) fn started(
        &mut self,
        id: &str,
        name: &str,
        parent: &str,
        description: &str,
    ) -> (Level, String) {
        self.names.insert(id.to_string(), name.to_string());
        (
            Level::Info,
            format!("[{name}] started by {parent}: {description}"),
        )
    }

    /// One of its loop events: a line for a tool it ran or a guard notice,
    /// nothing for its streaming.
    pub(crate) fn loop_event(&mut self, id: &str, ev: &LoopEvent) -> Option<(Level, String)> {
        let name = self.name(id);
        match ev {
            LoopEvent::ToolResult {
                call,
                output,
                duration_ms,
                ..
            } => {
                let line = result_line(call, output.is_error, *duration_ms);
                Some((Level::Info, format!("[{name}] {line}")))
            }
            LoopEvent::Guard { text } => Some((Level::Warning, format!("[{name}] ! {text}"))),
            _ => None,
        }
    }

    /// It stopped: finished, or stopped early (a warning).
    pub(crate) fn stopped(&mut self, id: &str, ok: bool) -> (Level, String) {
        let name = self.name(id);
        self.names.remove(id);
        if ok {
            (Level::Info, format!("[{name}] finished"))
        } else {
            (Level::Warning, format!("[{name}] stopped early"))
        }
    }
}

#[cfg(test)]
mod tests;
