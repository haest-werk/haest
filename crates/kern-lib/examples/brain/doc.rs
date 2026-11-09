//! brain's document view of a run: the model's text streams into the doc, and
//! the narration before a tool call ("Let me look that up") is taken back out
//! the moment the call starts, so the doc keeps only the answer.
//!
//! PLUG POINT: `LoopEvent::ToolCallStart`, through the host's `EventSink`.

use kern::{EventSink, LoopEvent, tool::ToolOutput};

#[derive(Default)]
pub struct DocSink {
    pub doc: String,
    /// Where the current model call's text began.
    step_start: usize,
    /// Every tool call id, as its start was seen.
    pub tool_starts: Vec<String>,
    pub events: Vec<LoopEvent>,
}

impl EventSink for DocSink {
    fn emit(&mut self, ev: LoopEvent) {
        match &ev {
            LoopEvent::ModelStart { .. } => self.step_start = self.doc.len(),
            LoopEvent::TextDelta { text } => self.doc.push_str(text),
            LoopEvent::ToolCallStart { id, .. } => {
                self.doc.truncate(self.step_start);
                self.tool_starts.push(id.clone());
            }
            _ => {}
        }
        self.events.push(ev);
    }
}

impl DocSink {
    /// The output of tool call `id`.
    pub fn output(&self, id: &str) -> &ToolOutput {
        self.events
            .iter()
            .find_map(|ev| match ev {
                LoopEvent::ToolResult { call, output, .. } if call.id == id => Some(output),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no result for tool call {id}"))
    }

    /// The guard nudged the model this turn.
    pub fn guarded(&self) -> bool {
        self.events
            .iter()
            .any(|ev| matches!(ev, LoopEvent::Guard { .. }))
    }
}
