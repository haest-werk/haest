//! Every loaded mod, in load order, and running one event across all of them.
//! Load order is event order: the first mod's `tool.before` decides first,
//! except a gate's ([`Mod::is_gate`], the approval mod), which judges after
//! every other mod has had its say, on the call as it will run.

use std::{fmt, sync::Arc};

use haest_core::{
    Tools,
    error::{BoxError, report},
    tool::{ToolContext, ToolError, ToolOutput},
};

use crate::api::{Before, Mod, ModEnv, ModRef, ModToolCall};

/// What the model reads for a call the human interrupted while a mod was deciding.
const INTERRUPTED: &str = "Interrupted by the user before this tool ran.";

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
/// every other mod has had its say, on the call as it will run. A mod that
/// fails in an event is reported through the host's
/// [`ModUi::error`](crate::ModUi::error) (unless the human interrupted
/// that tool call) and never takes the conversation down; what else happens
/// depends on the event:
///
/// | Event | A mod fails |
/// |---|---|
/// | `tool.before` | the call is refused and does not run: a guard that broke must not wave anything through |
/// | `tool.after` | the output stays as it was: the call happened, and the model must not retry it |
/// | `prompt.submit` | the text stays as it was |
/// | `system.prompt` | that mod adds nothing |
/// | a slash command | [`CommandOutcome::Handled`] |
///
/// Build one with [`load_mods`](crate::load_mods), or with [`Mods::builder`].
/// Once built, the list never changes. It is a cheap handle: give a clone
/// each to [`ModTools`](crate::ModTools),
/// [`ModSystemPrompt`](crate::ModSystemPrompt) and the host's input path,
/// and they all run the same mods for good.
#[derive(Clone)]
pub struct Mods(Arc<Loaded>);

/// What a [`Mods`] handle shares.
struct Loaded {
    env: ModEnv,
    list: Vec<ModRef>,
}

impl fmt::Debug for Mods {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mods")
            .field("env", &self.0.env)
            .field("list", &self.0.list)
            .finish()
    }
}

/// A [`Mods`] being put together: every mod, in load order, before any
/// clone of it exists ([`Mods::builder`]).
///
/// ```
/// # use haest_core::workspace::WorkspaceMode;
/// # use haest_mods::{ApprovalRequest, Decision, ModUi};
/// use haest_mods::{ApprovalMod, ModEnv, Mods, PlanMod};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
///
/// let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal);
/// let mods = Mods::builder(env)
///     .with(ApprovalMod::new())
///     .with(PlanMod::new())
///     .build();
/// assert_eq!(mods.names(), ["approval", "plan"]);
/// ```
#[must_use = "a builder does nothing until it is built"]
pub struct ModsBuilder(Loaded);

impl fmt::Debug for ModsBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModsBuilder")
            .field("env", &self.0.env)
            .field("list", &self.0.list)
            .finish()
    }
}

impl ModsBuilder {
    /// Add a mod after every mod already here (logged through
    /// [`ModUi::debug`](crate::ModUi::debug)): the mod itself, or an `Arc` of
    /// it the host keeps.
    pub fn with(mut self, m: impl Into<ModRef>) -> Self {
        let m = m.into();
        self.0.env.ui().debug(&format!("mod loaded: {}", m.name()));
        self.0.list.push(m);
        self
    }

    /// Add each mod, in order, as [`with`](Self::with) does.
    pub fn with_all(self, mods: impl IntoIterator<Item = impl Into<ModRef>>) -> Self {
        mods.into_iter().fold(self, Self::with)
    }

    /// The mods, fixed: every clone of the result runs these.
    #[must_use]
    pub fn build(self) -> Mods {
        Mods(Arc::new(self.0))
    }
}

impl Mods {
    /// No mods, in `env`.
    pub fn new(env: ModEnv) -> Self {
        Self::builder(env).build()
    }

    /// A list to add mods to, in `env`, built into a [`Mods`] that never
    /// changes ([`ModsBuilder`]).
    pub fn builder(env: ModEnv) -> ModsBuilder {
        ModsBuilder(Loaded {
            env,
            list: Vec::new(),
        })
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

    /// Every tool the mods add, unwrapped ([`crate::ModTools`] wraps them).
    pub fn tools(&self) -> Tools {
        self.list().iter().flat_map(|m| m.tools()).collect()
    }

    fn report(&self, m: &dyn Mod, event: &str, e: &BoxError) {
        let why = report(&**e);
        self.env()
            .ui()
            .error(&format!("Mod {} failed in {event}: {why}", m.name()));
    }

    /// `tool.before` across every mod: the call to run, or why it must not run.
    /// A mod that fails refuses the call -- a guard that broke must not wave
    /// anything through.
    pub(crate) async fn before(
        &self,
        mut call: ModToolCall,
        ctx: &ToolContext,
    ) -> Result<ModToolCall, ToolError> {
        let (gates, rest): (Vec<_>, Vec<_>) = self.list().iter().partition(|m| m.is_gate());
        for m in rest.into_iter().chain(gates) {
            match m.tool_before(&call, ctx, self.env()).await {
                Ok(Before::Pass) => {}
                Ok(Before::Input(input)) => call.input = input,
                Ok(Before::Deny(reason)) => {
                    return Err(ToolError::Input(format!("Refused: {reason}")));
                }
                Err(_) if ctx.cancel().is_cancelled() => {
                    return Err(ToolError::Input(INTERRUPTED.into()));
                }
                Err(e) => {
                    self.report(m.as_ref(), "tool.before", &e);
                    return Err(ToolError::Failed(format!(
                        "the mod {} failed before this call, so it did not run: {e}",
                        m.name()
                    )));
                }
            }
        }
        Ok(call)
    }

    /// `tool.after` across every mod. The tool DID run: a mod that fails leaves
    /// the output as it was, so the model never retries a call that happened.
    pub(crate) async fn after(
        &self,
        call: &ModToolCall,
        mut output: ToolOutput,
        ctx: &ToolContext,
    ) -> ToolOutput {
        for m in self.list() {
            match m.tool_after(call, &output, ctx, self.env()).await {
                Ok(Some(replaced)) => output = replaced,
                Ok(None) => {}
                Err(_) if ctx.cancel().is_cancelled() => {}
                Err(e) => self.report(m.as_ref(), "tool.after", &e),
            }
        }
        output
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
                Err(e) => self.report(m.as_ref(), "prompt.submit", &e),
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
                Ok(Some(text)) if !text.is_empty() => parts.push(text),
                Ok(_) => {}
                Err(e) => self.report(m.as_ref(), "system.prompt", &e),
            }
        }
        parts.join("\n\n")
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
    /// promise never settles never returns ([`ModCaps`](crate::ModCaps)
    /// stops code that runs too long, not code that waits). Cancel-safe: a
    /// host bounds it by dropping the call (a timeout, Ctrl-C), which stops
    /// the wait and leaves the mod's promise to settle on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// # use haest_core::workspace::WorkspaceMode;
    /// # use haest_mods::{ApprovalRequest, Decision, ModUi};
    /// use haest_mods::{CommandOutcome, ModEnv, Mods, PlanMod};
    /// # struct Terminal;
    /// # #[async_trait::async_trait]
    /// # impl ModUi for Terminal {
    /// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
    /// #     fn info(&self, _: &str) {}
    /// # }
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let env = ModEnv::new("/project", WorkspaceMode::Sandbox, Terminal);
    /// let mods = Mods::builder(env).with(PlanMod::new()).build();
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
