//! The host's model backend: a Provider of its own, no HTTP. Main and
//! subagents answer from separate scripts.

use super::*;

/// One request as the model saw it.
#[derive(Clone)]
pub struct Seen {
    pub sub: bool,
    pub system: String,
    pub tools: Vec<String>,
    pub messages: Vec<Message>,
}

/// Main and subagents answer from separate scripts (a subagent is told apart
/// by haest's subagent note in its system prompt). A subagent's call waits for
/// a permit from `sub_gate`, so a test decides when a background one finishes.
pub struct Scripted {
    pub main: Mutex<VecDeque<Vec<AssistantPart>>>,
    pub sub: Mutex<VecDeque<Vec<AssistantPart>>>,
    pub sub_gate: Arc<Semaphore>,
    pub seen: Mutex<Vec<Seen>>,
}

impl Scripted {
    pub fn requests(&self, sub: bool) -> Vec<Seen> {
        let seen = self.seen.lock().unwrap();
        seen.iter().filter(|s| s.sub == sub).cloned().collect()
    }
}

impl Provider for Scripted {
    fn id(&self) -> &str {
        "own"
    }
    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let sub = req.system.contains(SUBAGENT_MARK);
        self.seen.lock().unwrap().push(Seen {
            sub,
            system: req.system.clone(),
            tools: req.tools.iter().map(|t| t.name.clone()).collect(),
            messages: req.messages.clone(),
        });
        let script = if sub { &self.sub } else { &self.main };
        let parts = script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| say("(the script ran out)"));
        let (gate, model) = (self.sub_gate.clone(), req.model.clone());
        answer_stream(move |_| async move {
            if sub {
                gate.acquire().await.expect("the gate stays open").forget();
            }
            let mut m = AssistantMessage::start("own", &model);
            let calls = parts
                .iter()
                .any(|p| matches!(p, AssistantPart::ToolCall(_)));
            m.stop_reason = if calls {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            };
            m.content = parts;
            m.usage = Usage {
                cost_usd: Some(0.001),
                ..Default::default()
            };
            m
        })
    }
}

pub fn say(text: &str) -> Vec<AssistantPart> {
    vec![AssistantPart::Text { text: text.into() }]
}

pub fn call(id: &str, name: &str, input: Value) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
    })
}
