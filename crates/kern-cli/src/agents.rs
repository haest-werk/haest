//! Subagents in the CLI: the library's `SubagentHub` over this host (TS
//! `host/agent-hub.ts`). A subagent gets the conversation's deps, the
//! mod-wrapped base tools (Code included) behind its own deferral, the CLI's
//! system prompt for ITS model and tools plus what the mods append, and runs on
//! the conversation's current model and effort unless the call names one. Its
//! life shows as short lines on stderr (`notice` lines in --json).

use std::{collections::HashMap, sync::Arc, sync::Mutex};

use async_trait::async_trait;
use kern::{
    AgentDeps, Effort, LoopEvent, SystemPrompt,
    deferred_tools::DeferMode,
    mods::{ModSystemPrompt, Mods},
    tool::{Tool, ToolSource},
    tools::{
        Inbox,
        agents::{SubagentCaps, SubagentEvent, SubagentHost, SubagentHub},
    },
};

use crate::{
    host::Host,
    inbox::CliInbox,
    prompt::{AgentPrompt, CliPrompt},
    render::{Level, call_summary},
    tools::{Deferred, WrappedTools},
};

pub struct CliAgents {
    pub host: Arc<Host>,
    /// The conversation's deps. Their tools and prompt are left out (empty):
    /// the hub replaces both for every agent, and main's tools hold the hub.
    pub deps: AgentDeps,
    pub tools: Arc<WrappedTools>,
    pub prompt: Arc<CliPrompt>,
    pub mods: Arc<Mods>,
    pub defer: DeferMode,
    pub inbox: Arc<CliInbox>,
    pub log: Mutex<AgentLog>,
}

impl CliAgents {
    pub fn hub(self, caps: SubagentCaps) -> Arc<SubagentHub> {
        SubagentHub::new(Arc::new(self), caps)
    }

    fn say(&self, line: Option<(Level, String)>) {
        if let Some((level, text)) = line {
            self.host.term.lock().unwrap().notice(level, &text);
        }
    }
}

impl Inbox for CliAgents {
    fn submit(&self, text: String) {
        self.inbox.submit(text);
    }
}

#[async_trait]
impl SubagentHost for CliAgents {
    fn deps(&self) -> AgentDeps {
        self.deps.clone()
    }

    fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.tools()
    }

    async fn system_prompt(&self, model_spec: &str, tools: &[Arc<dyn Tool>]) -> String {
        let prompt = AgentPrompt {
            prompt: self.prompt.clone(),
            model: Some(model_spec.to_string()),
            tools: Arc::new(tools.to_vec()),
        };
        ModSystemPrompt::new(Arc::new(prompt), self.mods.clone())
            .system_prompt()
            .await
    }

    fn default_model(&self) -> (String, Option<Effort>) {
        (self.host.model(), self.host.effort())
    }

    fn tool_source(&self, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn ToolSource> {
        Arc::new(Deferred::new(tools, self.defer))
    }

    fn on_activity(&self, running: &[String]) {
        let line = self.log.lock().unwrap().activity(running.len());
        self.say(line.map(|l| (Level::Info, l)));
    }

    fn on_subagent(&self, id: &str, event: SubagentEvent) {
        let line = self.log.lock().unwrap().event(id, &event);
        self.say(line);
    }
}

/// How subagents read on the terminal: a line when one starts, one per tool
/// it ran and per guard notice, one when it stops, and the running count
/// when it changes. Its id is shown as its name.
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
                let mark = if output.is_error { "x" } else { "-" };
                let line = format!("[{name}] {mark} {} ({duration_ms} ms)", call_summary(call));
                Some((Level::Info, line))
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
