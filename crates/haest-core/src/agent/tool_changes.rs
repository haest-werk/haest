//! Tool changes that keep the prompt cache: the `tools` array a model that
//! takes mid-conversation tool changes keeps being sent (the baseline), and
//! the [`ToolChangeMessage`] that tells it what changed since.
//!
//! Where the baseline lives: in history, on the first change since the
//! conversation began or was compacted ([`ToolChangeMessage::baseline`]), so
//! a session file keeps it and a resumed conversation sends the same bytes.
//! Until a change records it, the agent remembers the tools its last answered
//! loop call sent to the model (`CacheState::tool_baseline`): a conversation
//! whose tools never change leaves history as it was.

use crate::{
    agent::Agent,
    messages::{AssistantMessage, AssistantPart, Message, ToolChange, ToolChangeMessage},
    provider::ToolSpec,
};

impl Agent {
    /// The `tools` array the next request sends, for a model that takes
    /// mid-conversation tool changes; None for any other, which is sent
    /// `current`, the tools on offer now.
    ///
    /// The baseline history records; else the one the last answered loop
    /// call to this same model spec sent; else `current`, which the call
    /// then starts the baseline with.
    pub(crate) fn tool_baseline(&self, current: &[ToolSpec]) -> Option<Vec<ToolSpec>> {
        let facts = self.deps().shared().facts().cache_facts(self.model_spec());
        if !facts.is_some_and(|f| f.tool_changes) {
            return None;
        }
        if let Some((baseline, _)) = recorded(self.history()) {
            return Some(baseline.to_vec());
        }
        let sent = self.cache.tool_baseline.as_ref();
        let sent = sent.filter(|(spec, _)| spec == self.model_spec());
        Some(sent.map_or_else(|| current.to_vec(), |(_, tools)| tools.clone()))
    }
}

/// The change a request of `messages` sent on `baseline` adds, `current`
/// being the tools on offer now: what differs between them and the tools the
/// baseline and the changes in `messages` offer. None without a baseline
/// (the model takes no tool changes), when nothing differs, or when the
/// request does not end on a user or tool message: the change waits for the
/// next request that does, since Anthropic takes one only right after a user
/// turn (tool results count) and right before an answer.
pub(crate) fn tool_change(
    baseline: Option<&[ToolSpec]>,
    current: &[ToolSpec],
    messages: &[Message],
) -> Option<ToolChangeMessage> {
    let baseline = baseline?;
    if !matches!(messages.last(), Some(Message::User(_) | Message::Tool(_))) {
        return None;
    }
    let recorded = recorded(messages);
    let offered = match &recorded {
        Some((_, changes)) => offered(baseline, changes),
        None => baseline.to_vec(),
    };
    let changes = diff(&offered, current, baseline);
    let first = recorded.is_none();
    (!changes.is_empty()).then(|| ToolChangeMessage {
        changes,
        baseline: first.then(|| baseline.to_vec()),
    })
}

/// Whether `answer` holds anything a provider sends back: a provider leaves
/// an empty answer out of later requests, and a tool change with no answer
/// after it would then sit right before the next user message, where
/// Anthropic refuses it. A change is recorded only with such an answer.
pub(crate) fn says_something(answer: &AssistantMessage) -> bool {
    answer.content.iter().any(|part| match part {
        AssistantPart::Text { text } => !text.is_empty(),
        AssistantPart::Thinking {
            signature,
            redacted,
            ..
        } => [signature, redacted]
            .iter()
            .any(|s| s.as_ref().is_some_and(|s| !s.is_empty())),
        AssistantPart::ToolCall(_) => true,
    })
}

/// The baseline `messages` record and every change from it on, oldest
/// first: the last tool change that carries a baseline starts them. None
/// when none does.
fn recorded(messages: &[Message]) -> Option<(&[ToolSpec], Vec<&ToolChange>)> {
    let changes = messages.iter().filter_map(|m| match m {
        Message::ToolChange(c) => Some(c),
        _ => None,
    });
    let mut found = None;
    for change in changes {
        if let Some(baseline) = &change.baseline {
            found = Some((baseline.as_slice(), Vec::new()));
        }
        if let Some((_, from)) = &mut found {
            from.extend(&change.changes);
        }
    }
    found
}

/// The tools `baseline` and then `changes` offer, in order: a tool put in
/// takes the place of the one of its name, or goes at the end.
fn offered(baseline: &[ToolSpec], changes: &[&ToolChange]) -> Vec<ToolSpec> {
    let mut tools = baseline.to_vec();
    for change in changes {
        let put = match change {
            ToolChange::Removed { name } => {
                tools.retain(|t| &t.name != name);
                continue;
            }
            ToolChange::Restored { name } => baseline.iter().find(|t| &t.name == name),
            ToolChange::Added { tool } => Some(tool),
        };
        let Some(tool) = put else { continue };
        match tools.iter_mut().find(|t| t.name == tool.name) {
            Some(held) => *held = tool.clone(),
            None => tools.push(tool.clone()),
        }
    }
    tools
}

/// What takes `offered` to `current`: each tool that went, by name, then
/// each tool that came or changed, in `current`'s order. One the baseline
/// holds as it is now, and that is not on offer, comes back by name
/// ([`ToolChange::Restored`]); any other by its definition, which replaces
/// one of its name.
fn diff(offered: &[ToolSpec], current: &[ToolSpec], baseline: &[ToolSpec]) -> Vec<ToolChange> {
    let named = |tools: &'_ [ToolSpec], name: &str| tools.iter().position(|t| t.name == name);
    let went = offered
        .iter()
        .filter(|t| named(current, &t.name).is_none())
        .map(|t| ToolChange::Removed {
            name: t.name.clone(),
        });
    let came = current
        .iter()
        .filter_map(|t| match named(offered, &t.name) {
            Some(at) if offered[at] == *t => None,
            None if baseline.contains(t) => Some(ToolChange::Restored {
                name: t.name.clone(),
            }),
            _ => Some(ToolChange::Added { tool: t.clone() }),
        });
    went.chain(came).collect()
}

#[cfg(test)]
mod tests;
