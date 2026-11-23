//! A file mod's loop events: what its handlers see of the agent (a small JSON
//! view, never the history: it crosses into the mod at every step) and what
//! their answer does to it. The answer comes back merged by bootstrap.js:
//! `{ compact?, steer?, stop? }` or `{ compact?, continue? }`.

use haest_core::{
    Agent, AssistantMessage, TurnOutcome,
    compaction::{CompactionOptions, compact},
    error::BoxError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::{BadReturn, JsMod};
use crate::{
    api::{Step, TurnEnd},
    events::ModEvent,
};

/// What one event's handlers decided, as bootstrap.js merged them.
#[derive(Debug, Default, Deserialize)]
struct Decided {
    /// Compact, with these instructions for the summary.
    compact: Option<Compact>,
    /// Steering, in the order given.
    #[serde(default)]
    steer: Vec<String>,
    /// End the turn, for this reason.
    stop: Option<String>,
    /// Keep the turn going with this text.
    #[serde(rename = "continue")]
    go_on: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Compact {
    instructions: Option<String>,
}

/// What every loop event's handler sees of `agent`: `modelSpec`, `effort`
/// (its word, or null) and `historyLength`.
fn view(agent: &Agent) -> Map<String, Value> {
    let mut v = Map::new();
    v.insert("modelSpec".into(), json!(agent.model_spec()));
    v.insert("effort".into(), json!(agent.effort()));
    v.insert("historyLength".into(), json!(agent.history().len()));
    v
}

/// A message as the session file writes it, less what is bulky and means
/// nothing to a mod: media bytes, thinking signatures, provider state.
fn light(msg: &impl Serialize) -> Value {
    let mut v = serde_json::to_value(msg).unwrap_or_default();
    let Some(o) = v.as_object_mut() else {
        return v;
    };
    o.remove("providerData");
    for list in ["media", "content"] {
        let parts = o.get_mut(list).and_then(Value::as_array_mut);
        for part in parts.into_iter().flatten().filter_map(Value::as_object_mut) {
            for bulky in ["data", "signature", "redacted"] {
                part.remove(bulky);
            }
        }
    }
    v
}

/// The summary a mod asked for: haest-core's compaction, keeping its usual
/// tail. The window is for deciding when to compact, which the mod did.
async fn compact_for(
    agent: &mut Agent,
    cancel: &CancellationToken,
    asked: Compact,
) -> Result<(), BoxError> {
    let facts = agent.deps().shared().facts().model_info(agent.model_spec());
    let cfg = CompactionOptions {
        instructions: asked.instructions,
        ..CompactionOptions::new(facts.map_or(0, |m| m.context_window))
    };
    compact(agent, cancel, &cfg).await?;
    Ok(())
}

impl JsMod {
    /// `event`'s handlers on `view`: what they decided, nothing when none is there.
    async fn decided(
        &self,
        event: ModEvent,
        view: Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<Decided, BoxError> {
        let got = self.event(event, Value::Object(view), Some(cancel)).await?;
        let decided: Option<Decided> =
            serde_json::from_value(got).map_err(|source| BadReturn::Loop { event, source })?;
        Ok(decided.unwrap_or_default())
    }

    /// Compact first, then steer, then stop: what a model event decided.
    async fn apply(
        agent: &mut Agent,
        cancel: &CancellationToken,
        decided: Decided,
    ) -> Result<Step, BoxError> {
        if let Some(asked) = decided.compact {
            compact_for(agent, cancel, asked).await?;
        }
        for text in decided.steer {
            agent.control().steer(text);
        }
        Ok(decided.stop.map_or(Step::Pass, Step::Stop))
    }

    pub(super) async fn on_model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<Step, BoxError> {
        if !self.hooks(ModEvent::ModelBefore) {
            return Ok(Step::Pass);
        }
        let mut v = view(agent);
        let last = agent.history().last().map_or(Value::Null, light);
        v.insert("lastMessage".into(), last);
        let decided = self.decided(ModEvent::ModelBefore, v, cancel).await?;
        Self::apply(agent, cancel, decided).await
    }

    /// No compaction here: the answer's tool results are still to land.
    pub(super) async fn on_model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
    ) -> Result<Step, BoxError> {
        if !self.hooks(ModEvent::ModelAfter) {
            return Ok(Step::Pass);
        }
        let mut v = view(agent);
        v.insert("answer".into(), light(answer));
        let decided = self.decided(ModEvent::ModelAfter, v, cancel).await?;
        let decided = Decided {
            compact: None,
            ..decided
        };
        Self::apply(agent, cancel, decided).await
    }

    pub(super) async fn on_turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
    ) -> Result<TurnEnd, BoxError> {
        if !self.hooks(ModEvent::TurnEnd) {
            return Ok(TurnEnd::Pass);
        }
        let mut v = view(agent);
        v.insert("stopReason".into(), json!(outcome.stop_reason));
        v.insert("error".into(), json!(outcome.error));
        let decided = self.decided(ModEvent::TurnEnd, v, cancel).await?;
        if let Some(asked) = decided.compact {
            compact_for(agent, cancel, asked).await?;
        }
        Ok(decided.go_on.map_or(TurnEnd::Pass, TurnEnd::Continue))
    }
}

#[cfg(test)]
mod tests {
    use haest_core::{
        Message,
        messages::{AssistantPart, MediaKind, MediaPart, UserMessage},
        testing::text_answer,
    };

    use super::light;

    #[test]
    fn a_message_crosses_without_media_bytes_signatures_or_provider_state() {
        let photo = MediaPart {
            kind: MediaKind::Image,
            media_type: "image/png".into(),
            data: "QUJD".repeat(1_000),
            name: "photo.png".into(),
        };
        let asked = Message::User(UserMessage {
            content: "what is this?".into(),
            media: vec![photo],
        });
        let v = light(&asked);
        assert_eq!(v["content"], "what is this?");
        assert_eq!(v["media"][0]["name"], "photo.png");
        assert!(v["media"][0].get("data").is_none());

        let mut answer = text_answer("a cat");
        answer.content.insert(
            0,
            AssistantPart::Thinking {
                thinking: "looks furry".into(),
                signature: Some("sig".repeat(100)),
                redacted: Some("opaque".into()),
            },
        );
        answer.provider_data = Some(serde_json::json!({"reasoning": "opaque"}));
        let v = light(&answer);
        assert_eq!(v["content"][0]["thinking"], "looks furry");
        assert_eq!(v["content"][1]["text"], "a cat");
        for gone in [
            &v["content"][0]["signature"],
            &v["content"][0]["redacted"],
            &v["providerData"],
        ] {
            assert!(gone.is_null(), "{v}");
        }
    }
}
