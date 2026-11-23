//! Every loaded mod, in load order, and running one event across all of them.
//! Load order is event order: the first mod's `tool.before` decides first,
//! except a gate's ([`Mod::is_gate`], the approval mod), which judges after
//! every other mod has had its say, on the call as it will run.

use std::sync::Arc;

use haest_core::{Tools, error::BoxError};

use crate::{
    api::{Mod, ModRef},
    decision::system_text,
    env::ModEnv,
    events::ModEvent,
};

mod call;
mod nested;
mod turn;

use nested::Reported;

/// What [`Mods::command`] did with a slash command. More outcomes may come
/// (a command that asks the host to act): a `match` has a `_` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandOutcome {
    /// No mod has this command: the host handles the line (or says it is unknown).
    Unknown,
    /// The mod handled the line itself, or failed (reported to the human);
    /// nothing goes to the model.
    Handled,
    /// The text to send to the model, as if the human had typed it.
    Prompt(String),
}

/// Every mod one conversation loaded, in load order, and the environment they
/// all see.
///
/// Load order is event order: the first mod's `tool.before` decides first,
/// except a gate's ([`Mod::is_gate`], the approval mod), which judges after
/// every other mod has had its say, on the call as it will run. In the loop
/// events the first mod that stops the turn (`model.before`,
/// `model.after`) or keeps it going (`turn.end`) wins, and the mods after it
/// are not asked. A mod that fails in an event is reported through the
/// host's [`ModUi::error`](crate::ModUi::error) (unless the human
/// interrupted that tool call or turn) and never takes the conversation
/// down; what else happens depends on the event:
///
/// | Event | A mod fails |
/// |---|---|
/// | `tool.before` | the call is refused and does not run: a guard that broke must not wave anything through |
/// | `tool.after` | the output stays as it was: the call happened, and the model must not retry it |
/// | `prompt.submit` | the text stays as it was |
/// | `system.prompt` | that mod adds nothing |
/// | `model.before`, `model.after` | that mod changes nothing, the mods after it are asked, and the turn goes on |
/// | `turn.end` | that mod keeps nothing going, the mods after it are asked, and the turn ends unless one of them keeps it going |
/// | a slash command | [`CommandOutcome::Handled`] |
///
/// Build one with [`load_mods`](crate::load_mods), or with [`Mods::new`].
/// Once built, the list never changes. It is a cheap handle: give a clone
/// each to [`ModTools`](crate::ModTools),
/// [`ModSystemPrompt`](crate::ModSystemPrompt),
/// [`ModLoopHooks`](crate::ModLoopHooks) and the host's input path, and they
/// all run the same mods for good.
///
/// A `Mods` and its clones belong to one conversation at a time: they share
/// its [`ModEnv`] and what its turns left there (a `turn.end` that kept a
/// turn going, the session the host named). Give each conversation its own.
///
/// A `Mods` is a [`Mod`] itself: inside another, its mods run in its place
/// by these same rules, and at each event its env takes what the outer
/// env holds. What changes while a conversation runs lives inside
/// one mod that way (mod files that reload are a `Mods` their bridge swaps
/// whole), and the conversation's own list stays as it was built. An inner
/// `Mods` is one conversation's too: two that take turns with it hand its
/// mods each one's own session, but two that run it at once race for its
/// env.
#[derive(Clone, Debug)]
pub struct Mods(Arc<Loaded>);

/// What a [`Mods`] handle shares.
#[derive(Debug)]
struct Loaded {
    env: ModEnv,
    list: Vec<ModRef>,
}

impl Mods {
    /// `mods`, in load order, in `env`, each logged through
    /// [`ModUi::debug`](crate::ModUi::debug): a mod itself, or an `Arc` of
    /// one the host keeps.
    ///
    /// ```
    /// # use haest_core::workspace::WorkspaceMode;
    /// # use haest_mods::{ApprovalDecision, ApprovalRequest, ModUi};
    /// use haest_mods::{ApprovalMod, ModEnv, ModRef, Mods, PlanMod};
    /// # struct Terminal;
    /// # #[async_trait::async_trait]
    /// # impl ModUi for Terminal {
    /// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> ApprovalDecision { ApprovalDecision::Deny }
    /// #     fn info(&self, _: &str) {}
    /// # }
    ///
    /// let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal);
    /// let mods = Mods::new(env, [ModRef::new(ApprovalMod::new()), ModRef::new(PlanMod::new())]);
    /// assert_eq!(mods.names(), ["approval", "plan"]);
    /// ```
    pub fn new(env: ModEnv, mods: impl IntoIterator<Item = impl Into<ModRef>>) -> Self {
        let list: Vec<ModRef> = mods.into_iter().map(Into::into).collect();
        for m in &list {
            env.ui().debug(&format!("mod loaded: {}", m.name()));
        }
        Self(Arc::new(Loaded { env, list }))
    }

    /// The environment every mod's events get.
    pub fn env(&self) -> &ModEnv {
        &self.0.env
    }

    fn list(&self) -> &[ModRef] {
        &self.0.list
    }

    /// Every mod's name, in load order.
    pub fn names(&self) -> Vec<String> {
        self.list().iter().map(|m| m.name().to_string()).collect()
    }

    /// Every tool the mods add, unwrapped ([`crate::ModTools`] wraps them),
    /// hidden ones included.
    pub fn tools(&self) -> Tools {
        self.list().iter().flat_map(|m| m.tools()).collect()
    }

    /// The names of every tool a mod hides ([`Mod::disabled_tools`]), once
    /// each, in load order: what [`ModTools`](crate::ModTools) leaves out. A
    /// host that offers tools outside [`ModTools`](crate::ModTools) leaves
    /// these out too.
    pub fn disabled_tools(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in self.list().iter().flat_map(|m| m.disabled_tools()) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// `m` failed in `event`: the human sees it, unless `m` is a [`Mods`]
    /// whose own mod failed, which reported it already.
    fn report(&self, m: &dyn Mod, event: &str, e: &BoxError) {
        if !e.is::<Reported>() {
            self.env().report_failure(m.name(), event, &**e);
        }
    }

    /// A human message after every `prompt.submit` hook, in load order, each
    /// seeing the text the one before left. A mod that fails leaves the text
    /// as it was. Call it on the human's text before it goes to the agent.
    pub async fn rewrite_prompt(&self, text: &str) -> String {
        let mut text = text.to_string();
        for m in self.list() {
            match m.prompt_submit(&text, self.env()).await {
                Ok(Some(rewritten)) => text = rewritten,
                Ok(None) => {}
                Err(e) => self.report(m.as_ref(), ModEvent::PromptSubmit.as_str(), &e),
            }
        }
        text
    }

    /// What every `system.prompt` hook appends, in load order, joined by a
    /// blank line; empty when none adds anything. [`ModSystemPrompt`](crate::ModSystemPrompt)
    /// puts it after the host's prompt.
    pub async fn system_append(&self) -> String {
        let mut parts = Vec::new();
        for m in self.list() {
            match m.system_prompt(self.env()).await {
                Ok(text) => parts.extend(text),
                Err(e) => self.report(m.as_ref(), ModEvent::SystemPrompt.as_str(), &e),
            }
        }
        system_text(parts)
    }

    /// Every slash command the mods answer, without the `/`, once each.
    pub fn commands(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in self.list().iter().flat_map(|m| m.commands()) {
            let name = bare(&name).to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// The mod that answers `name`: a later mod's command replaces an earlier one's.
    fn command_owner(&self, name: &str) -> Option<&ModRef> {
        let name = bare(name);
        self.list()
            .iter()
            .rev()
            .find(|m| m.commands().iter().any(|c| bare(c) == name))
    }

    /// Whether a mod answers the slash command `name` (with or without its `/`).
    pub fn has_command(&self, name: &str) -> bool {
        self.command_owner(name).is_some()
    }

    /// Run a mod's slash command: `name` with or without its `/`, `args` the
    /// rest of the human's line.
    ///
    /// When two mods answer the same name, the one loaded later runs. A host
    /// sends [`CommandOutcome::Prompt`] to the model as the human's message.
    ///
    /// A command takes as long as its mod does: a mod file's command whose
    /// promise never settles never returns (a mod file's `VmCaps::slice`
    /// stops code that runs too long, not code that waits). Cancel-safe: a
    /// host bounds it by dropping the call (a timeout, Ctrl-C), which stops
    /// the wait and leaves the mod's promise to settle on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// # use haest_core::workspace::WorkspaceMode;
    /// # use haest_mods::{ApprovalDecision, ApprovalRequest, ModUi};
    /// use haest_mods::{CommandOutcome, ModEnv, Mods, PlanMod};
    /// # struct Terminal;
    /// # #[async_trait::async_trait]
    /// # impl ModUi for Terminal {
    /// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> ApprovalDecision { ApprovalDecision::Deny }
    /// #     fn info(&self, _: &str) {}
    /// # }
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let env = ModEnv::new("/project", WorkspaceMode::Sandbox, Terminal);
    /// let mods = Mods::new(env, [PlanMod::new()]);
    ///
    /// assert_eq!(mods.commands(), ["plan"]);
    /// assert_eq!(mods.command("/plan", "").await, CommandOutcome::Handled);
    /// assert_eq!(mods.command("/deploy", "now").await, CommandOutcome::Unknown);
    /// # }
    /// ```
    pub async fn command(&self, name: &str, args: &str) -> CommandOutcome {
        let Some(m) = self.command_owner(name) else {
            return CommandOutcome::Unknown;
        };
        match m.command(bare(name), args, self.env()).await {
            Ok(Some(text)) => CommandOutcome::Prompt(text),
            Ok(None) => CommandOutcome::Handled,
            Err(e) => {
                self.report(m.as_ref(), &format!("/{}", bare(name)), &e);
                CommandOutcome::Handled
            }
        }
    }
}

fn bare(name: &str) -> &str {
    name.strip_prefix('/').unwrap_or(name)
}

#[cfg(test)]
mod tests;
