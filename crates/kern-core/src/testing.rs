//! Fakes for tests, in this crate and every crate that builds on it
//! (`kern-core = { ..., features = ["testing"] }` in dev-dependencies).

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use futures::stream;
use serde_json::{Map, Value};

use crate::{
    messages::{AssistantMessage, AssistantPart, StopReason, ToolCall, Usage},
    provider::{Provider, ProviderEvent, ProviderRequest, ProviderStream, ResolvedModel},
};

/// A provider that answers with scripted messages, in order, and records every request.
/// Out of script = an error answer, so a loop that calls too often fails loudly.
#[derive(Default)]
pub struct ScriptedProvider {
    script: Mutex<VecDeque<AssistantMessage>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl ScriptedProvider {
    pub fn new(script: impl IntoIterator<Item = AssistantMessage>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::default(),
        })
    }

    pub fn push(&self, msg: AssistantMessage) {
        self.script.lock().unwrap().push_back(msg);
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<ProviderRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// A resolver that serves every spec with this provider (model = the spec's model part, or the spec).
    pub fn resolver(
        self: &Arc<Self>,
    ) -> impl Fn(&str) -> Result<ResolvedModel, String> + Send + Sync + 'static {
        let p = self.clone();
        move |spec: &str| {
            let model = spec.split_once(':').map_or(spec, |(_, m)| m).to_string();
            Ok(ResolvedModel {
                provider: p.clone(),
                model,
            })
        }
    }
}

impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let model = req.model.clone();
        self.requests.lock().unwrap().push(req);
        let msg = self.script.lock().unwrap().pop_front().unwrap_or_else(|| {
            let mut m = AssistantMessage::start("scripted", &model);
            m.stop_reason = StopReason::Error;
            m.error_message = Some("scripted provider: out of script".into());
            m
        });
        let mut events = Vec::new();
        for part in &msg.content {
            match part {
                AssistantPart::Text { text } => events.push(ProviderEvent::TextDelta(text.clone())),
                AssistantPart::Thinking { thinking, .. } => {
                    events.push(ProviderEvent::ThinkingDelta(thinking.clone()))
                }
                AssistantPart::ToolCall(c) => events.push(ProviderEvent::ToolCallStart {
                    id: c.id.clone(),
                    name: c.name.clone(),
                }),
            }
        }
        events.push(ProviderEvent::Done(msg));
        Box::pin(stream::iter(events))
    }
}

/// An assistant answer with text only.
pub fn text_answer(text: &str) -> AssistantMessage {
    let mut m = AssistantMessage::start("scripted", "m");
    m.content.push(AssistantPart::Text { text: text.into() });
    m.usage = Usage {
        input: 10,
        output: 5,
        ..Default::default()
    };
    m
}

/// An assistant answer that calls tools: `(id, name, input)`.
pub fn tool_answer(calls: &[(&str, &str, Value)]) -> AssistantMessage {
    let mut m = AssistantMessage::start("scripted", "m");
    for (id, name, input) in calls {
        let input: Map<String, Value> = input.as_object().cloned().unwrap_or_default();
        m.content.push(AssistantPart::ToolCall(ToolCall {
            id: (*id).into(),
            name: (*name).into(),
            input,
        }));
    }
    m.stop_reason = StopReason::ToolUse;
    m.usage = Usage {
        input: 10,
        output: 5,
        ..Default::default()
    };
    m
}
