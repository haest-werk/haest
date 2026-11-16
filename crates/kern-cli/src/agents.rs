//! How subagents read on the terminal: a line when one starts, one per tool
//! it ran and per guard notice, one when it stops, and the running count
//! when it changes. Its id is shown as its name.

use std::collections::HashMap;

use kern::{LoopEvent, tools::agents::SubagentEvent};

use crate::render::{Level, result_line};

#[derive(Debug, Default)]
pub struct AgentLog {
    names: HashMap<String, String>,
    running: usize,
}

impl AgentLog {
    pub fn activity(&mut self, running: usize) -> Option<String> {
        if running == self.running {
            return None;
        }
        self.running = running;
        Some(format!("Subagents running: {running}"))
    }

    pub fn event(&mut self, id: &str, event: &SubagentEvent) -> Option<(Level, String)> {
        let name = self
            .names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string());
        match event {
            SubagentEvent::Started {
                name,
                parent,
                description,
            } => {
                self.names.insert(id.to_string(), name.clone());
                Some((
                    Level::Info,
                    format!("[{name}] started by {parent}: {description}"),
                ))
            }
            SubagentEvent::Loop(LoopEvent::ToolResult {
                call,
                output,
                duration_ms,
                ..
            }) => {
                let line = result_line(call, output.is_error, *duration_ms);
                Some((Level::Info, format!("[{name}] {line}")))
            }
            SubagentEvent::Loop(LoopEvent::Guard { text }) => {
                Some((Level::Warning, format!("[{name}] ! {text}")))
            }
            SubagentEvent::Stopped { ok } => {
                self.names.remove(id);
                Some(match ok {
                    true => (Level::Info, format!("[{name}] finished")),
                    false => (Level::Warning, format!("[{name}] stopped early")),
                })
            }
            SubagentEvent::Loop(_) | SubagentEvent::InnerCall(_) => None,
        }
    }
}

#[cfg(test)]
mod tests;
