//! A host that writes down what the loop hooks were asked, and answers as a
//! test tells it; a tool that writes down that it ran, in the same log.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::FnTool;
use crate::{
    agent::Agent,
    deps::AgentHooks,
    error::BoxError,
    events::TurnOutcome,
    messages::{AssistantMessage, Message, Usage},
    tool::{ToolOutput, ToolRef},
};

/// What an [`Asking`] host was asked and a [`logging_tool`] ran, in order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Asked {
    /// `after_model_call`: the answer's call ids; whether it was last in history.
    Answer { calls: Vec<String>, last: bool },
    /// `before_turn_end`.
    TurnEnd,
    /// A call of the logging tool ran, by id.
    Tool(String),
}

pub(crate) type AskedLog = Arc<Mutex<Vec<Asked>>>;

/// A host that writes down every ask of the loop hooks and answers as told.
/// `after_model_call` pays `spend` outside the loop, aborts the turn on
/// `abort`, fails with `refuse`. `before_turn_end` says `always`, else the
/// next of `go_on`, else None.
#[derive(Default)]
pub(crate) struct Asking {
    pub seen: AskedLog,
    pub spend: Option<Usage>,
    pub abort: bool,
    pub refuse: Option<&'static str>,
    pub always: Option<&'static str>,
    pub go_on: Mutex<Vec<&'static str>>,
}

#[async_trait]
impl AgentHooks for Asking {
    async fn after_model_call(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        _: &CancellationToken,
    ) -> Result<(), BoxError> {
        let last = matches!(agent.history().last(), Some(Message::Assistant(a)) if a == answer);
        let calls = answer.tool_calls().map(|c| c.id.clone()).collect();
        self.seen
            .lock()
            .unwrap()
            .push(Asked::Answer { calls, last });
        if let Some(usage) = &self.spend {
            agent.spent_outside_the_loop(usage);
        }
        if self.abort {
            agent.control().abort();
        }
        self.refuse.map_or(Ok(()), |why| Err(why.into()))
    }
    async fn before_turn_end(
        &self,
        _: &mut Agent,
        _: &TurnOutcome,
        _: &CancellationToken,
    ) -> Result<Option<String>, BoxError> {
        self.seen.lock().unwrap().push(Asked::TurnEnd);
        let mut go_on = self.go_on.lock().unwrap();
        let next = (!go_on.is_empty()).then(|| go_on.remove(0));
        Ok(self.always.or(next).map(String::from))
    }
}

/// `Log`: writes its call id to `seen` and answers `logged`.
pub(crate) fn logging_tool(seen: &AskedLog, read_only: bool) -> ToolRef {
    let seen = seen.clone();
    FnTool::arc("Log", read_only, move |_, ctx| {
        let seen = seen.clone();
        async move {
            let id = ctx.tool_call_id.unwrap_or_default();
            seen.lock().unwrap().push(Asked::Tool(id));
            Ok(ToolOutput::text("logged"))
        }
    })
}
