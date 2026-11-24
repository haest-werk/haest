//! A loaded session's history told the client again, as `session/load`
//! asks: the user's messages, the agent's, every tool call and its result,
//! then the task list.

use std::path::Path;

use agent_client_protocol::v1::schema::{SessionUpdate, ToolCallStatus, ToolCallUpdate};
use haest_core::{AssistantPart, Message, ToolSourceRef, task_board::Task};

use crate::{
    tell::{Teller, agent_text, agent_thought, user_text},
    translate::{new_call, plan, result_fields},
};

/// Tell `history` (and the task list, `tasks`) to the client as updates of
/// the session `tell` speaks for; `tools` say each call's kind.
pub(crate) fn replay(
    tell: &Teller,
    history: &[Message],
    tasks: &[Task],
    tools: &ToolSourceRef,
    cwd: &Path,
) {
    let callable = tools.callable();
    let kind = |name: &str| {
        callable
            .get(name)
            .map(|t| t.kind().to_string())
            .unwrap_or_default()
    };
    for message in history {
        match message {
            Message::User(m) => tell.tell(user_text(m.content.clone())),
            Message::Assistant(m) => {
                for part in &m.content {
                    match part {
                        AssistantPart::Text { text } if !text.is_empty() => {
                            tell.tell(agent_text(text.clone()));
                        }
                        AssistantPart::Thinking { thinking, .. } if !thinking.is_empty() => {
                            tell.tell(agent_thought(thinking.clone()));
                        }
                        AssistantPart::ToolCall(call) => {
                            let entry =
                                new_call(call, &kind(&call.name), cwd, ToolCallStatus::InProgress);
                            tell.tell(SessionUpdate::ToolCall(entry));
                        }
                        _ => {}
                    }
                }
            }
            Message::Tool(m) => {
                let fields = result_fields(&kind(&m.tool_name), m.is_error, &m.content, None, None);
                let update = ToolCallUpdate::new(m.tool_call_id.clone(), fields);
                tell.tell(SessionUpdate::ToolCallUpdate(update));
            }
            // A change to the tool list, and a message newer than this crate.
            _ => {}
        }
    }
    if !tasks.is_empty() {
        tell.tell(SessionUpdate::Plan(plan(tasks)));
    }
}
