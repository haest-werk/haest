//! A [`Mods`] inside another: one mod whose events run every mod it holds,
//! by the rules of [`Mods`] (order, gates, the first deny, stop or continue
//! wins), in its place. A bridge whose mods change while a conversation
//! runs (mod files that reload) holds them in a `Mods` it swaps whole, and
//! the conversation's own `Mods` never changes.

use std::{error::Error, fmt};

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, Tools, TurnOutcome,
    error::BoxError,
    tool::{ToolContext, ToolOutput},
};
use tokio_util::sync::CancellationToken;

use super::{
    CommandOutcome, Mods,
    call::{INTERRUPTED, Judged},
};
use crate::{
    api::{Before, Mod, ModToolCall, Step, TurnEnd},
    env::ModEnv,
};

/// The name a [`Mods`] goes by as a mod of another.
const NAME: &str = "mods";

/// A mod failed inside a nested [`Mods`], which reported it: that mod's
/// own name and error, which the `Mods` around it passes on as they are,
/// without reporting them again.
#[derive(Debug)]
pub(super) struct Reported {
    pub(super) name: String,
    pub(super) error: BoxError,
}

impl fmt::Display for Reported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl Error for Reported {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.error.source()
    }
}

/// A `Mods` is a mod too: put one inside another and its mods run where it
/// stands, each event by the rules [`Mods`] has, so the outer `Mods` sees
/// what it would see were they its own. A mod inside that fails is
/// reported once, under its own name; one that stops the turn ends it in
/// its own name. Inside, its mods get the [`ModEnv`] it was built with,
/// holding at each event what the outer one's does: whether a turn was
/// kept going, and the session the conversation is on. It goes by `mods`
/// in a report, and is no gate: its own gates judge last among its mods.
#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl Mod for Mods {
    fn name(&self) -> &str {
        NAME
    }

    fn is_gate(&self) -> bool {
        false
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        ctx: &ToolContext,
        haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        self.env().follow(haest);
        match self.judge(call.clone(), ctx).await {
            Judged::Run(run) if run.input == call.input => Ok(Before::Pass),
            Judged::Run(run) => Ok(Before::Input(run.input)),
            Judged::Refused(reason) => Ok(Before::Deny(reason)),
            // The call's own token fired: the outer `Mods` sees it too.
            Judged::Interrupted => Err(INTERRUPTED.into()),
            Judged::Failed(failed) => Err(Box::new(failed)),
        }
    }

    async fn tool_after(
        &self,
        call: &ModToolCall,
        output: &ToolOutput,
        ctx: &ToolContext,
        haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        self.env().follow(haest);
        Ok(Some(self.after(call, output.clone(), ctx).await))
    }

    async fn prompt_submit(&self, text: &str, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        self.env().follow(haest);
        Ok(Some(self.rewrite_prompt(text).await))
    }

    async fn system_prompt(&self, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        self.env().follow(haest);
        let added = self.system_append().await;
        Ok((!added.is_empty()).then_some(added))
    }

    async fn model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        self.env().follow(haest);
        Mods::model_before(self, agent, cancel).await?;
        Ok(Step::Pass)
    }

    async fn model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        self.env().follow(haest);
        Mods::model_after(self, agent, answer, cancel).await?;
        Ok(Step::Pass)
    }

    /// Its mods see whether the outer mods already kept the turn going.
    async fn turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<TurnEnd, BoxError> {
        self.env().follow(haest);
        let goes_on = Mods::turn_end(self, agent, outcome, cancel).await;
        Ok(goes_on.map_or(TurnEnd::Pass, TurnEnd::Continue))
    }

    fn tools(&self) -> Tools {
        Mods::tools(self)
    }

    fn disabled_tools(&self) -> Vec<String> {
        Mods::disabled_tools(self)
    }

    fn commands(&self) -> Vec<String> {
        Mods::commands(self)
    }

    /// A command none of its mods answers any more (they changed since the
    /// outer `Mods` looked) is a failure saying so.
    async fn command(
        &self,
        name: &str,
        args: &str,
        _haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        match Mods::command(self, name, args).await {
            CommandOutcome::Prompt(text) => Ok(Some(text)),
            CommandOutcome::Handled => Ok(None),
            CommandOutcome::Unknown => Err(format!("no mod answers /{name} any more").into()),
        }
    }
}

#[cfg(test)]
mod tests;
