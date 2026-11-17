//! The hub tests' provider: main and subagents answer from separate scripts
//! (a subagent is told apart by its system prompt), every call at a fixed
//! price, and what each subagent call was offered is recorded.

use std::sync::Mutex;

use futures::future::BoxFuture;
use haest_core::{
    AssistantMessage, AssistantPart, StopReason, ToolCall, Usage,
    provider::{Provider, ProviderEvent, ProviderRequest, ProviderStream},
};
use serde_json::Value;

pub(super) type Script =
    Box<dyn Fn(ProviderRequest, usize) -> BoxFuture<'static, Vec<AssistantPart>> + Send + Sync>;

/// A script that answers at once.
pub(super) fn now(
    f: impl Fn(&ProviderRequest, usize) -> Vec<AssistantPart> + Send + Sync + 'static,
) -> Script {
    Box::new(move |req, n| {
        let parts = f(&req, n);
        Box::pin(async move { parts })
    })
}

pub(super) fn say(text: &str) -> Vec<AssistantPart> {
    vec![AssistantPart::Text { text: text.into() }]
}

pub(super) fn call(id: &str, name: &str, input: Value) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
    })
}

/// Main's script: one Agent call with `input`, then `then` on every later call.
pub(super) fn spawns(input: Value, then: &'static str) -> Script {
    now(move |_, n| match n {
        0 => vec![call("a", "Agent", input.clone())],
        _ => say(then),
    })
}

/// Every call costs $0.01.
pub(super) const CALL_USD: f64 = 0.01;

pub(super) struct Roles {
    pub(super) main: Script,
    pub(super) sub: Script,
    pub(super) calls: Mutex<(usize, usize)>,
    pub sub_tools: Mutex<Vec<Vec<String>>>,
    pub sub_systems: Mutex<Vec<String>>,
}

impl Provider for Roles {
    fn id(&self) -> &str {
        "fake"
    }
    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let is_sub = req.system.contains("# You are a subagent");
        let n = {
            let mut c = self.calls.lock().unwrap();
            let slot = if is_sub { &mut c.1 } else { &mut c.0 };
            *slot += 1;
            *slot - 1
        };
        if is_sub {
            let names = req.tools.iter().map(|t| t.name.clone()).collect();
            self.sub_tools.lock().unwrap().push(names);
            self.sub_systems.lock().unwrap().push(req.system.clone());
        }
        let (model, cancel) = (req.model.clone(), req.cancel.clone());
        let parts = (if is_sub { &self.sub } else { &self.main })(req, n);
        Box::pin(futures::stream::once(async move {
            let mut m = AssistantMessage::start("fake", &model);
            tokio::select! {
                content = parts => {
                    let tools = content.iter().any(|p| matches!(p, AssistantPart::ToolCall(_)));
                    m.stop_reason = if tools { StopReason::ToolUse } else { StopReason::EndTurn };
                    m.content = content;
                    m.usage = Usage { cost_usd: Some(CALL_USD), ..Default::default() };
                }
                _ = cancel.cancelled() => m.stop_reason = StopReason::Aborted,
            }
            ProviderEvent::Done(m)
        }))
    }
}
