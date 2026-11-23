//! Fakes for tests, in this crate and every crate that builds on it
//! (`haest-core = { ..., features = ["testing"] }` in dev-dependencies).
//!
//! [`ScriptedProvider`] stands in for a model: it answers each call with the
//! next scripted [`AssistantMessage`] and records every request, so a test
//! drives a whole [`Agent`](crate::Agent) turn offline and then checks what
//! the loop sent. [`text_answer`] and [`tool_answer`] build the script.
//!
//! Not part of the API and not covered by semver, like every `testing`
//! module and feature in haest: for `dev-dependencies` only, it changes in
//! any release.
//!
//! # Examples
//!
//! A turn with one tool call, offline: the script asks for the tool, then
//! answers; the test checks what the tool returned and what the model was
//! sent.
//!
//! ```
//! use haest_core::{
//!     Agent, AgentDeps, AgentOptions, LoopEvent, SharedDeps, ShutWorkspace, StopReason, Tool,
//!     Tools,
//!     testing::{ScriptedProvider, text_answer, tool_answer},
//! };
//! use serde_json::json;
//! # use async_trait::async_trait;
//! # use haest_core::{JsonMap, ToolContext, ToolError, ToolOutput, req_str};
//! # use serde_json::Value;
//! # struct Shout;
//! # #[async_trait]
//! # impl Tool for Shout {
//! #     fn name(&self) -> &str { "Shout" }
//! #     fn description(&self) -> String { "Returns the text in capitals.".into() }
//! #     fn parameters(&self) -> Value { json!({"type": "object"}) }
//! #     fn kind(&self) -> &str { "shout" }
//! #     async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
//! #         Ok(ToolOutput::text(req_str(&input, "text")?.to_uppercase()))
//! #     }
//! # }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_core::BoxError> {
//! let model = ScriptedProvider::new([
//!     tool_answer(&[("call-1", "Shout", json!({"text": "hello"}))]),
//!     text_answer("It says HELLO."),
//! ]);
//! let shared = SharedDeps::new(model.resolver(), "/", ShutWorkspace);
//! let deps = AgentDeps::new(shared, Tools::new().with(Shout), "You shout.");
//! let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
//!
//! let mut events: Vec<LoopEvent> = Vec::new();
//! let end = agent.run("Shout hello", &mut events).await;
//! assert_eq!(end.stop_reason, StopReason::EndTurn);
//! let result = events.iter().find_map(|e| match e {
//!     LoopEvent::ToolResult { output, .. } => Some(output.content.clone()),
//!     _ => None,
//! });
//! assert_eq!(result.as_deref(), Some("HELLO"));
//!
//! // What the loop sent: the model, the prompt and the tool, by name.
//! let sent = model.requests();
//! assert_eq!((sent[0].model.as_str(), sent[0].system.as_str()), ("m", "You shout."));
//! assert_eq!(sent[0].tools[0].name, "Shout");
//! # Ok(())
//! # }
//! ```

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use futures::stream;
use serde_json::{Map, Value};

use crate::{
    cache::PrewarmRefused,
    messages::{AssistantMessage, AssistantPart, StopReason, ToolCall, Usage},
    provider::{
        ModelError, Provider, ProviderEvent, ProviderRequest, ProviderStream, ResolvedModel,
    },
};

/// A provider that answers with scripted messages, in order, and records every request.
/// Out of script = an error answer, so a loop that calls too often fails loudly.
///
/// Each answer streams like a real one: a delta or a tool-call start per
/// part, then [`ProviderEvent::Done`] with the message as scripted. Its id is
/// `scripted`. It ignores the request's `cancel`, and takes every cache
/// pre-warm ([`Agent::warm_cache`](crate::Agent::warm_cache)).
///
/// A cheap `Clone`: every clone answers from the same script and records
/// into the same list, so a test keeps one to read [`requests`](Self::requests)
/// while the agent holds another.
#[derive(Clone, Default)]
pub struct ScriptedProvider(Arc<Script>);

#[derive(Default)]
struct Script {
    answers: Mutex<VecDeque<AssistantMessage>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

/// Only this file's code runs under either lock, so a poisoned one is a bug
/// here.
impl Script {
    fn answers(&self) -> MutexGuard<'_, VecDeque<AssistantMessage>> {
        self.answers.lock().expect("the script's lock poisoned")
    }

    fn requests(&self) -> MutexGuard<'_, Vec<ProviderRequest>> {
        self.requests.lock().expect("the script's lock poisoned")
    }
}

/// How many answers are left and how many requests came in.
impl fmt::Debug for ScriptedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedProvider")
            .field("answers_left", &self.0.answers().len())
            .field("requests", &self.0.requests().len())
            .finish()
    }
}

impl ScriptedProvider {
    /// A provider that gives these answers, one per call, in order.
    pub fn new(script: impl IntoIterator<Item = AssistantMessage>) -> Self {
        Self(Arc::new(Script {
            answers: Mutex::new(script.into_iter().collect()),
            requests: Mutex::default(),
        }))
    }

    /// Add an answer at the end of the script.
    pub fn push(&self, msg: AssistantMessage) {
        self.0.answers().push_back(msg);
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<ProviderRequest> {
        self.0.requests().clone()
    }

    /// A resolver that serves every spec with this provider (model = the spec's model part, or the spec).
    pub fn resolver(
        &self,
    ) -> impl Fn(&str) -> Result<ResolvedModel, ModelError> + Send + Sync + 'static {
        let p = self.clone();
        move |spec: &str| {
            let model = spec.split_once(':').map_or(spec, |(_, m)| m);
            Ok(ResolvedModel::new(p.clone(), model))
        }
    }
}

impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let model = req.model.clone();
        self.0.requests().push(req);
        let msg = self.0.answers().pop_front().unwrap_or_else(|| {
            let mut m = AssistantMessage::start("scripted", &model);
            m.stop_reason = StopReason::Error;
            m.error_message = Some("scripted provider: out of script".into());
            m
        });
        let mut events: Vec<ProviderEvent> = (msg.content.iter())
            .map(|part| match part {
                AssistantPart::Text { text } => ProviderEvent::TextDelta(text.clone()),
                AssistantPart::Thinking { thinking, .. } => {
                    ProviderEvent::ThinkingDelta(thinking.clone())
                }
                AssistantPart::ToolCall(c) => ProviderEvent::ToolCallStart {
                    id: c.id.clone(),
                    name: c.name.clone(),
                },
            })
            .collect();
        events.push(ProviderEvent::Done(msg));
        Box::pin(stream::iter(events))
    }

    /// Every request goes out as a pre-warm when asked: the next answer in
    /// the script is its reply.
    fn check_prewarm(&self, _req: &ProviderRequest) -> Result<(), PrewarmRefused> {
        Ok(())
    }
}

/// An assistant answer with text only: provider `scripted`, model `m`, stop
/// reason `end_turn`, usage 10 tokens in and 5 out.
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

/// An assistant answer that calls tools: `(id, name, input)`, in order.
/// `input` must be a JSON object (anything else becomes `{}`). Stop reason
/// `tool_use`, usage as [`text_answer`]'s.
///
/// # Examples
///
/// ```
/// use haest_core::{StopReason, testing::tool_answer};
/// use serde_json::json;
///
/// let answer = tool_answer(&[("call-1", "Read", json!({"path": "/project/README.md"}))]);
/// assert_eq!(answer.stop_reason, StopReason::ToolUse);
/// assert_eq!(answer.tool_calls().next().ok_or("no call")?.name, "Read");
/// # Ok::<(), &'static str>(())
/// ```
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

/// Panics unless `e`'s [`report`](crate::error::report) says each cause
/// under it once: no error in the chain prints its own source in its text
/// (the rule in [`error`](crate::error)).
///
/// # Examples
///
/// ```should_panic
/// use haest_core::testing::assert_causes_said_once;
///
/// /// Prints its source and also hands it out as the source: said twice.
/// #[derive(Debug, thiserror::Error)]
/// #[error("reading the file: {source}")]
/// struct Twice {
///     source: std::io::Error,
/// }
///
/// assert_causes_said_once(&Twice { source: std::io::Error::other("denied") });
/// ```
///
/// # Panics
///
/// When the text of a cause in `e`'s chain is said other than once in the
/// report: that is the failure it tests for.
pub fn assert_causes_said_once(e: &dyn std::error::Error) {
    let line = crate::error::report(e);
    let mut cause = e.source();
    while let Some(c) = cause {
        let text = c.to_string();
        let said = line.matches(text.as_str()).count();
        assert!(
            text.is_empty() || said == 1,
            "\"{text}\" is said {said} times in: {line}"
        );
        cause = c.source();
    }
}
