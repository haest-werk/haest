//! The human's Allow / Deny with a memory: the generic half of a host's
//! [`crate::ModUi::ask`]. The host supplies the prompt itself ([`Prompter`]: a
//! terminal question, a control panel dialog); the gate keeps the rules every
//! host must keep:
//!   - ALWAYS is scoped to the ask it was pressed on -- the same tool with the
//!     same key (a Bash ask's key is its exact command) -- never the whole tool:
//!     one ALWAYS on `rm -rf node_modules` must not wave through `rm -rf ~`.
//!     Those asks then answer themselves, reported through
//!     [`Prompter::auto_approved`] so the receipt trail stays complete;
//!   - an unanswered ask is a DENY after the timeout, and on interrupt -- a tool
//!     call never hangs forever on a human who walked away;
//!   - an ask under an already-interrupted call is a deny at once, never a prompt.

use std::{
    collections::HashSet,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::api::{ApprovalRequest, Decision, ModUi};

/// How the human answered one prompt: what a [`Prompter`] returns. More
/// answers may come (for this session only, deny always): a `match` has a
/// `_` arm, which denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApprovalAnswer {
    /// Allow this one call.
    Allow,
    /// Allow, and every later ask with the same tool and key.
    AllowAlways,
    /// Refuse this one call.
    Deny,
}

/// PLUG POINT: puts one ask in front of the human -- a terminal question, a
/// dialog, a push notification -- for an [`ApprovalGate`].
///
/// The gate keeps the rules (ALWAYS, the timeout, interrupts); a prompter
/// only asks and reports what the human pressed. The gate holds it in a
/// [`PrompterRef`].
#[async_trait]
pub trait Prompter: Send + Sync {
    /// Ask and wait for the answer. The future is dropped when the ask is
    /// withdrawn (interrupt, timeout): a late answer must go nowhere, so take
    /// the prompt down when it is dropped.
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer;
    /// An ask an earlier ALWAYS answered, without a prompt: show it, so the
    /// human's record of what ran stays complete. Default: nothing.
    fn auto_approved(&self, _req: &ApprovalRequest) {}
}

haest_core::plug_ref! {
    /// A [`Prompter`], shared.
    pub struct PrompterRef(dyn Prompter);
}

/// How long an [`ApprovalGate`] waits for an answer by default (10 minutes)
/// before the ask is a Deny.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The human's Allow / Deny with a memory: the rules every host's
/// [`ModUi::ask`](crate::ModUi::ask) must keep, over the host's own
/// [`Prompter`].
///
/// - ALWAYS is scoped to the ask it was pressed on: the same
///   [`tool_name`](ApprovalRequest::tool_name) and the same
///   [`key`](ApprovalRequest::key) (the description when there is none; a
///   Bash ask's key is its exact command). One ALWAYS on `rm -rf node_modules`
///   never waves through `rm -rf ~`.
/// - An ask nobody answers is a Deny after the timeout ([`ASK_TIMEOUT`] unless
///   set with [`ApprovalGate::with_timeout`]).
/// - An interrupt is a Deny, and an ask under a call already interrupted is a
///   Deny at once, never a prompt.
///
/// The ALWAYS answers live as long as the gate: one gate per conversation
/// keeps them for that conversation. A clone is the same gate (the same
/// prompter, the same ALWAYS answers).
///
/// [`mod_ui`](Self::mod_ui) makes the gate the host's whole [`ModUi`]: asks
/// through the gate, every other line to one function. The host keeps the
/// gate itself to [`revoke`](Self::revoke) an ALWAYS.
///
/// # Examples
///
/// A host built on the gate, with [`ApprovalMod`](crate::ApprovalMod) asking
/// before a Bash command that can destroy work.
///
/// ```
/// use std::sync::mpsc;
///
/// use async_trait::async_trait;
/// use haest_core::{ShutWorkspace, ToolContext, Tools, tool::input, workspace::WorkspaceMode};
/// use haest_mods::{
///     ApprovalAnswer, ApprovalGate, ApprovalMod, ApprovalRequest, ModEnv, ModLine, ModTools, Mods,
///     Prompter,
/// };
/// use serde_json::json;
/// # struct Bash;
/// # #[async_trait]
/// # impl haest_core::Tool for Bash {
/// #     fn name(&self) -> &str { "Bash" }
/// #     fn description(&self) -> String { "Run a command".into() }
/// #     fn parameters(&self) -> serde_json::Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "shell.exec" }
/// #     async fn execute(&self, i: haest_core::tool::JsonMap, _: &ToolContext)
/// #         -> Result<haest_core::ToolOutput, haest_core::ToolError> {
/// #         Ok(haest_core::ToolOutput::text(format!("ran {}", i["command"])))
/// #     }
/// # }
///
/// /// The human: presses ALWAYS to clear a build directory and Deny on
/// /// anything else, and tells the host every prompt it saw.
/// struct Human(mpsc::Sender<String>);
///
/// #[async_trait]
/// impl Prompter for Human {
///     async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
///         let _ = self.0.send(req.input_preview.clone());
///         if req.input_preview.ends_with("build") {
///             ApprovalAnswer::AllowAlways
///         } else {
///             ApprovalAnswer::Deny
///         }
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let (human, prompts) = mpsc::channel();
/// // How the mods reach the human: asks through the gate, lines to the terminal.
/// let ui = ApprovalGate::new(Human(human)).mod_ui(|line| match line {
///     ModLine::Debug(_) => {}
///     line => println!("{}", line.text()),
/// });
/// let env = ModEnv::new("/project", WorkspaceMode::Real, ui);
/// let mods = Mods::builder(env).with(ApprovalMod::new()).build();
/// let tools = ModTools::wrap(&mods, Tools::new().with(Bash));
///
/// let ctx = ToolContext::new("/project", ShutWorkspace);
/// let bash = |command: &str| input(json!({ "command": command }));
/// tools[0].execute(bash("ls")?, &ctx).await?; // nothing to ask
/// tools[0].execute(bash("rm -rf build")?, &ctx).await?; // asked: ALWAYS
/// tools[0].execute(bash("rm -rf build")?, &ctx).await?; // the same command: no prompt
/// assert!(tools[0].execute(bash("rm -rf ~")?, &ctx).await.is_err()); // asked again, denied
/// assert_eq!(prompts.try_iter().collect::<Vec<_>>(), ["rm -rf build", "rm -rf ~"]);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ApprovalGate {
    prompter: PrompterRef,
    always: Arc<Mutex<HashSet<String>>>,
    timeout: Duration,
}

impl fmt::Debug for ApprovalGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let always = self.always.lock().map_or(0, |a| a.len());
        f.debug_struct("ApprovalGate")
            .field("prompter", &self.prompter)
            .field("always", &always)
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn rule_key(req: &ApprovalRequest) -> String {
    let key = req.key.as_deref().unwrap_or(&req.description);
    format!("{}\n{key}", req.tool_name)
}

impl ApprovalGate {
    /// A gate over `prompter` (the host's value, or an `Arc` of it the host
    /// keeps), with no ALWAYS answers yet and the default [`ASK_TIMEOUT`].
    pub fn new(prompter: impl Into<PrompterRef>) -> Self {
        Self {
            prompter: prompter.into(),
            always: Arc::default(),
            timeout: ASK_TIMEOUT,
        }
    }

    /// The gate with another timeout: how long an ask waits for the human
    /// before it is a Deny.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The human's decision on `req`.
    ///
    /// An ask an earlier ALWAYS covers is an Allow at once (reported through
    /// [`Prompter::auto_approved`]). Otherwise the prompter asks; the ask is
    /// withdrawn -- its future dropped -- and a Deny returned when `cancel`
    /// fires or the timeout passes. A `cancel` that fired before the call is a
    /// Deny with no prompt at all.
    pub async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        // Checked first: a prompt raised now would only ever be withdrawn.
        if cancel.is_cancelled() {
            return Decision::Deny;
        }
        let key = rule_key(&req);
        if self.lock_always().contains(&key) {
            self.prompter.auto_approved(&req);
            return Decision::Allow;
        }
        let answer = tokio::select! {
            a = self.prompter.prompt(&req) => a,
            _ = cancel.cancelled() => ApprovalAnswer::Deny,
            _ = tokio::time::sleep(self.timeout) => ApprovalAnswer::Deny,
        };
        match answer {
            ApprovalAnswer::Allow => Decision::Allow,
            ApprovalAnswer::AllowAlways => {
                self.lock_always().insert(key);
                Decision::Allow
            }
            ApprovalAnswer::Deny => Decision::Deny,
        }
    }

    /// Forgets every ALWAYS given for `tool_name` (the human took them back):
    /// every ask of that tool prompts again.
    pub fn revoke(&self, tool_name: &str) {
        let prefix = format!("{tool_name}\n");
        self.lock_always().retain(|k| !k.starts_with(&prefix));
    }

    /// The ALWAYS answers, held until the guard drops. Never held while the
    /// prompter runs, so a poisoned one is a bug here.
    fn lock_always(&self) -> MutexGuard<'_, HashSet<String>> {
        self.always
            .lock()
            .expect("the approval gate's ALWAYS lock poisoned")
    }

    /// The host's whole [`ModUi`]: [`ask`](ModUi::ask) through this gate,
    /// and every [`info`](ModUi::info), [`warn`](ModUi::warn),
    /// [`error`](ModUi::error) and [`debug`](ModUi::debug) line to `lines`,
    /// as a [`ModLine`] that says which.
    /// The gate stays the host's: an ALWAYS it [`revoke`](Self::revoke)s is
    /// gone for the `ModUi` too. The [class example](Self#examples) uses it.
    pub fn mod_ui<F>(&self, lines: F) -> impl ModUi + use<F>
    where
        F: Fn(ModLine<'_>) + Send + Sync + 'static,
    {
        GateUi {
            gate: self.clone(),
            lines,
        }
    }
}

/// A line a mod sends through the host's [`ModUi`], for the function given
/// to [`ApprovalGate::mod_ui`]: one case per line method.
///
/// More kinds of line may come, as [`ModUi`] gains line methods: a `match`
/// has a `_` arm, which still shows the line ([`text`](Self::text)).
///
/// ```
/// use haest_mods::ModLine;
///
/// fn show(line: ModLine<'_>) -> String {
///     match line {
///         ModLine::Error(text) => format!("error: {text}"),
///         ModLine::Debug(_) => String::new(),
///         other => other.text().to_string(),
///     }
/// }
/// assert_eq!(show(ModLine::Warn("held")), "held");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModLine<'a> {
    /// [`ModUi::info`]: a line the human sees.
    Info(&'a str),
    /// [`ModUi::warn`]: something was skipped or held; the human should see it.
    Warn(&'a str),
    /// [`ModUi::error`]: a mod failed to load or in an event; the human must see it.
    Error(&'a str),
    /// [`ModUi::debug`]: for the host's diagnostic log; the human need not see it.
    Debug(&'a str),
}

impl<'a> ModLine<'a> {
    /// The line's text, whichever kind it is.
    pub fn text(&self) -> &'a str {
        match *self {
            ModLine::Info(text) | ModLine::Warn(text) | ModLine::Error(text) => text,
            ModLine::Debug(message) => message,
        }
    }
}

/// An [`ApprovalGate`] and where the other lines go: the [`ModUi`] that
/// [`ApprovalGate::mod_ui`] makes.
struct GateUi<F> {
    gate: ApprovalGate,
    lines: F,
}

#[async_trait]
impl<F> ModUi for GateUi<F>
where
    F: Fn(ModLine<'_>) + Send + Sync + 'static,
{
    async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        self.gate.ask(req, cancel).await
    }

    fn info(&self, text: &str) {
        (self.lines)(ModLine::Info(text));
    }

    fn warn(&self, text: &str) {
        (self.lines)(ModLine::Warn(text));
    }

    fn error(&self, text: &str) {
        (self.lines)(ModLine::Error(text));
    }

    fn debug(&self, message: &str) {
        (self.lines)(ModLine::Debug(message));
    }
}

#[cfg(test)]
mod tests;
