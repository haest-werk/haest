//! One inner call's transcript row: `start` when it is admitted, `end` when it
//! answers. The end is owed from the moment the start went out -- a run that is
//! stopped drops its in-flight calls, and their rows still close, as interrupted
//! calls, so no transcript shows a call that never ends.

use std::{sync::Arc, time::Instant};

use haest_core::{
    messages::ToolCall,
    tool::{InnerCallEvent, ToolHost, ToolOutput},
};

pub(crate) struct InnerRow {
    host: Arc<dyn ToolHost>,
    parent_id: String,
    call: Option<ToolCall>,
    started: Instant,
}

impl InnerRow {
    pub fn start(host: Arc<dyn ToolHost>, parent_id: &str, call: &ToolCall) -> Self {
        host.on_inner_call(InnerCallEvent::Start {
            parent_id: parent_id.to_string(),
            call: call.clone(),
        });
        Self {
            host,
            parent_id: parent_id.to_string(),
            call: Some(call.clone()),
            started: Instant::now(),
        }
    }

    pub fn end(mut self, output: &ToolOutput) {
        self.send(output.clone());
    }

    fn send(&mut self, output: ToolOutput) {
        if let Some(call) = self.call.take() {
            self.host.on_inner_call(InnerCallEvent::End {
                parent_id: self.parent_id.clone(),
                call,
                output,
                duration_ms: self.started.elapsed().as_millis() as u64,
            });
        }
    }
}

impl Drop for InnerRow {
    fn drop(&mut self) {
        self.send(ToolOutput::error(
            "Interrupted: the Code run ended before this call did.",
        ));
    }
}
