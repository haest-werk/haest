//! [`Agent::warm_cache`]: the last request again, as a pre-warm.

use tokio_util::sync::CancellationToken;

use super::{WarmError, WarmOutcome};
use crate::{
    agent::Agent,
    deps::NoHooks,
    media::fit_for_model,
    messages::{Message, StopReason},
    session::{interrupted, split_pending},
};

/// The history a pre-warm sends: what the next turn starts from (a call a
/// dropped turn left is answered as that turn would answer it), up to its
/// last user or tool message. That is where the last request ended and put
/// its rolling breakpoint; the answers after it are left out, since a
/// request that ends on an answer asks the model to continue it.
pub(super) fn warm_messages(history: &[Message]) -> Vec<Message> {
    let repaired = split_pending(history.to_vec());
    let mut messages = repaired.messages;
    let answered = repaired.pending.into_iter().map(interrupted);
    messages.extend(answered.map(Message::Tool));
    while matches!(messages.last(), Some(Message::Assistant(_))) {
        messages.pop();
    }
    messages
}

impl Agent {
    /// Sends the conversation's last request again as a cache pre-warm, so
    /// the provider's prompt cache holds it for the next turn: the system
    /// prompt, tools, effort, cache TTL and history the next model call
    /// would send, ending where the last request ended (its last user or
    /// tool message), with nothing after it. The provider generates as
    /// little as it can (Anthropic: `max_tokens: 0`, not streamed, nothing;
    /// OpenRouter's `anthropic/` models: `max_tokens: 1`, one token).
    ///
    /// It changes no history and emits no event. What it cost goes on the
    /// conversation's ledger, and toward the running turn's caps when one
    /// runs, as compaction's summary does; [`WarmOutcome::usage`] and
    /// [`WarmError::Failed`] report it. [`WarmOutcome::cache`] says whether
    /// it read the cache (it stays warm) or wrote it (it had gone cold, or
    /// the prefix changed). [`ping_pays`](super::ping_pays) says whether
    /// another one is worth sending. Media the model cannot take is withheld
    /// as the loop withholds it, without telling the hooks again.
    ///
    /// # Errors
    ///
    /// Nothing was sent for [`WarmError::NoRequest`] (no user or tool
    /// message in history), [`WarmError::Model`] (the spec resolves to no
    /// provider) and [`WarmError::Refused`] (the provider sends no pre-warm,
    /// or the model thinks on a token budget). [`WarmError::Failed`]: the
    /// pre-warm was sent and failed, or `cancel` fired.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "testing")]
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), haest_core::BoxError> {
    /// use haest_core::{
    ///     Agent, AgentDeps, AgentOptions, AssistantMessage, LoopEvent, SharedDeps, ShutWorkspace,
    ///     Tools,
    ///     cache::WarmCache,
    ///     testing::{ScriptedProvider, text_answer},
    /// };
    /// use tokio_util::sync::CancellationToken;
    ///
    /// // The turn's answer, then the pre-warm's: nothing said, the prefix read.
    /// let mut ping = AssistantMessage::start("scripted", "m");
    /// ping.usage.cache_read = 4_000;
    /// let provider = ScriptedProvider::new([text_answer("hi"), ping]);
    /// let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
    /// let mut agent = Agent::new(AgentDeps::new(shared, Tools::new(), "Be brief."),
    ///     AgentOptions::new("scripted:m"));
    /// agent.run("hello", &mut Vec::<LoopEvent>::new()).await;
    ///
    /// let warmed = agent.warm_cache(&CancellationToken::new()).await?;
    /// assert_eq!(warmed.cache, WarmCache::Read);
    /// assert_eq!(agent.history().len(), 2, "history is untouched");
    /// let sent = provider.requests();
    /// assert!(sent[1].prewarm);
    /// assert_eq!(sent[1].messages, sent[0].messages, "the last request again");
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "testing"))]
    /// # fn main() {}
    /// ```
    pub async fn warm_cache(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<WarmOutcome, WarmError> {
        let messages = warm_messages(self.history());
        if messages.is_empty() {
            return Err(WarmError::NoRequest);
        }
        let shared = self.deps().shared();
        let resolved = shared.models().resolve(self.model_spec())?;
        let model = resolved.model().to_string();
        let accepts = shared.facts().input_modalities(self.model_spec());
        let messages = fit_for_model(&messages, accepts.as_ref(), &model, &NoHooks);
        let prefix = self.request_prefix(&model).await;
        let req = prefix
            .request(model, messages, cancel.clone())
            .with_prewarm(true);
        if let Err(source) = resolved.provider().check_prewarm(&req) {
            return Err(WarmError::Refused {
                model_spec: self.model_spec().to_string(),
                source,
            });
        }
        let answer = resolved.complete(req).await;
        self.spent_outside_the_loop(&answer.usage);
        match answer.stop_reason {
            StopReason::Error | StopReason::Aborted => Err(WarmError::Failed {
                reason: answer
                    .error_message
                    .unwrap_or_else(|| "cancelled".to_string()),
                usage: answer.usage,
            }),
            _ => Ok(WarmOutcome::from_usage(answer.usage)),
        }
    }
}
