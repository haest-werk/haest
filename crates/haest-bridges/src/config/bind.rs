//! The `hooks` setting bound: each entry a [`ShellHook`] or an [`HttpHook`]
//! in a [`Binding`] with its matcher, timeout and `on_error`, all in one
//! [`Hooks`], as a Rust host binds them by hand.

use std::{fmt, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use haest_core::{ChildEnv, error::BoxError};
#[cfg(feature = "http")]
use haest_http::HttpClientRef;
use haest_mods::{
    Binding, Event, Hook, Hooks, ModEvent, ModelAfterPayload, ModelBeforePayload,
    PromptSubmitPayload, SystemPromptPayload, ToolAfterPayload, ToolBeforePayload, ToolEvent,
    TurnEndPayload,
};
use tokio_util::sync::CancellationToken;

use super::{HookDialect, HookEntry, HookSettings, Target, entry_name};
#[cfg(feature = "http")]
use crate::HttpHook;
#[cfg(feature = "shell")]
use crate::ShellHook;
use crate::{Claude, Haest};

/// What the [`Hooks`] are named in a report (`Mod [hooks] failed in
/// tool.before: hook 2 on tool.before failed: ..`): the setting they come
/// from.
const NAME: &str = "[hooks]";

/// The `claude` dialect as of an event.
type ClaudeNow = Arc<dyn Fn() -> Claude + Send + Sync>;

/// What a host hands the hooks its `hooks` setting binds
/// ([`HookSettings::hooks`]): the conversation's directory, the variables a
/// command gets (or that no process may start), the HTTP client a POST goes
/// through, and the session the `claude` dialect names.
///
/// ```
/// use haest_bridges::{Claude, HookHost};
/// use haest_core::ChildEnv;
///
/// let path = std::env::var("PATH").unwrap_or_default();
/// let host = HookHost::new("/work/project")
///     .with_env(ChildEnv::new([("PATH", path)]))
///     .with_claude(|| Claude::new().with_session_id("conversation-7"));
/// ```
#[derive(Clone)]
pub struct HookHost {
    cwd: PathBuf,
    env: Option<ChildEnv>,
    #[cfg(feature = "http")]
    client: HttpClientRef,
    claude: ClaudeNow,
}

impl HookHost {
    /// Hooks of a conversation in `cwd`: a command runs there, and the
    /// `claude` dialect names it as `cwd`. No process may start until
    /// [`with_env`](Self::with_env); a POST goes through haest-http's
    /// default client; the `claude` dialect names no session.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            env: None,
            #[cfg(feature = "http")]
            client: HttpClientRef::default(),
            claude: Arc::new(Claude::new),
        }
    }

    /// A command may start, with `env` and nothing else of the host's
    /// environment: what haest gives every process it starts. Without it a
    /// command hook is not bound, and a note says so: a host that hands
    /// haest its own files starts no process.
    #[must_use]
    pub fn with_env(mut self, env: ChildEnv) -> Self {
        self.env = Some(env);
        self
    }

    /// A POST goes through `client`, the host's own.
    #[cfg(feature = "http")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http")))]
    #[must_use]
    pub fn with_client(mut self, client: impl Into<HttpClientRef>) -> Self {
        self.client = client.into();
        self
    }

    /// The `claude` dialect as of each event: what `claude` returns then,
    /// so a hook names the session the conversation is on at that moment
    /// (a new one after the human's /clear), its id and its transcript.
    #[must_use]
    pub fn with_claude(mut self, claude: impl Fn() -> Claude + Send + Sync + 'static) -> Self {
        self.claude = Arc::new(claude);
        self
    }
}

impl fmt::Debug for HookHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookHost")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

impl HookSettings {
    /// Every entry bound in one [`Hooks`], named `[hooks]` in reports: a
    /// `command` as a [`ShellHook`], a `url` as an [`HttpHook`], each in the
    /// [`Binding`] of its event with its matcher, timeout and `on_error`,
    /// in the dialect it names. An event's entries bind in the order
    /// written, so a report's `hook 2 on tool.before` is the second
    /// `[[hooks."tool.before"]]` across the config's layers. None when no
    /// entry binds.
    ///
    /// An entry that cannot bind here is left out with a note, the second
    /// value: a command where no process may start
    /// ([`HookHost::with_env`]), a command without this crate's `shell`
    /// feature, a URL without `http`.
    pub fn hooks(&self, host: &HookHost) -> (Option<Hooks>, Vec<String>) {
        let mut hooks = Hooks::new().with_name(NAME);
        let (mut bound, mut notes) = (0, Vec::new());
        for (event, entries) in &self.events {
            for (nth, entry) in (1..).zip(entries) {
                match bind(hooks.clone(), *event, entry, host) {
                    Ok(more) => {
                        hooks = more;
                        bound += 1;
                    }
                    Err(why) => {
                        notes.push(format!("{} is not bound: {why}", entry_name(*event, nth)))
                    }
                }
            }
        }
        ((bound > 0).then_some(hooks), notes)
    }
}

/// `entry` bound to `event` in `hooks`, or why it cannot be here.
fn bind(
    hooks: Hooks,
    event: ModEvent,
    entry: &HookEntry,
    host: &HookHost,
) -> Result<Hooks, String> {
    let carried = carried(entry, host)?;
    match entry.dialect {
        HookDialect::Claude => on(
            hooks,
            event,
            entry,
            InClaude {
                carried,
                claude: host.claude.clone(),
            },
        ),
        // `haest`, and a dialect newer than this match: the event's own JSON.
        _ => on(hooks, event, entry, carried),
    }
}

/// The hook that carries `entry`'s event, in haest's dialect.
fn carried(entry: &HookEntry, host: &HookHost) -> Result<Carried, String> {
    match &entry.target {
        #[cfg(feature = "shell")]
        Target::Command(command) => {
            let Some(env) = &host.env else {
                return Err(
                    "a command hook needs a host that starts processes, and this one starts none"
                        .into(),
                );
            };
            let hook = ShellHook::new(command.as_str())
                .with_cwd(&host.cwd)
                .with_env(env.clone());
            Ok(Carried::Shell(hook))
        }
        #[cfg(not(feature = "shell"))]
        Target::Command(_) => Err("a command hook needs haest-bridges' `shell` feature".into()),
        #[cfg(feature = "http")]
        Target::Url(url) => {
            let hook = HttpHook::new(url.as_str())
                .with_client(host.client.clone())
                .with_cwd(&host.cwd);
            let hook =
                (entry.headers.iter()).fold(hook, |h, (name, value)| h.with_header(name, value));
            Ok(Carried::Http(hook))
        }
        #[cfg(not(feature = "http"))]
        Target::Url(_) => Err("an HTTP hook needs haest-bridges' `http` feature".into()),
    }
}

/// `hook` bound to `event` in `hooks` with `entry`'s options.
fn on<H>(hooks: Hooks, event: ModEvent, entry: &HookEntry, hook: H) -> Result<Hooks, String>
where
    H: Hook<ToolBeforePayload>
        + Hook<ToolAfterPayload>
        + Hook<PromptSubmitPayload>
        + Hook<SystemPromptPayload>
        + Hook<ModelBeforePayload>
        + Hook<ModelAfterPayload>
        + Hook<TurnEndPayload>,
{
    Ok(match event {
        ModEvent::ToolBefore => {
            hooks.on_tool_before(tool_binding::<ToolBeforePayload>(hook, entry))
        }
        ModEvent::ToolAfter => hooks.on_tool_after(tool_binding::<ToolAfterPayload>(hook, entry)),
        ModEvent::PromptSubmit => {
            hooks.on_prompt_submit(binding::<PromptSubmitPayload>(hook, entry))
        }
        ModEvent::SystemPrompt => {
            hooks.on_system_prompt(binding::<SystemPromptPayload>(hook, entry))
        }
        ModEvent::ModelBefore => hooks.on_model_before(binding::<ModelBeforePayload>(hook, entry)),
        ModEvent::ModelAfter => hooks.on_model_after(binding::<ModelAfterPayload>(hook, entry)),
        ModEvent::TurnEnd => hooks.on_turn_end(binding::<TurnEndPayload>(hook, entry)),
        // An event newer than this match: the tests fail until it is here.
        other => return Err(format!("{other} can't be bound from the config yet")),
    })
}

/// `hook` with `entry`'s timeout and `on_error`, else its event's defaults.
fn binding<E: Event>(hook: impl Hook<E>, entry: &HookEntry) -> Binding<E> {
    let mut binding = Binding::new(hook);
    if let Some(timeout) = entry.timeout {
        binding = binding.with_timeout(timeout);
    }
    if let Some(on_error) = entry.on_error {
        binding = binding.with_on_error(on_error);
    }
    binding
}

/// [`binding`], and the tools `entry`'s matcher picks.
fn tool_binding<E: ToolEvent>(hook: impl Hook<E>, entry: &HookEntry) -> Binding<E> {
    let binding = binding(hook, entry);
    match &entry.matcher {
        Some(pattern) => binding.with_matcher(pattern),
        None => binding,
    }
}

/// A command or a POST in haest's dialect: every event.
#[derive(Clone)]
enum Carried {
    #[cfg(feature = "shell")]
    Shell(ShellHook<Haest>),
    #[cfg(feature = "http")]
    Http(HttpHook<Haest>),
}

#[async_trait]
impl<E: Event> Hook<E> for Carried {
    async fn run(&self, payload: E, cancel: &CancellationToken) -> Result<E::Decision, BoxError> {
        match self {
            #[cfg(feature = "shell")]
            Carried::Shell(hook) => hook.run(payload, cancel).await,
            #[cfg(feature = "http")]
            Carried::Http(hook) => hook.run(payload, cancel).await,
        }
    }
}

/// A command or a POST in Claude Code's dialect, made as of each event
/// ([`HookHost::with_claude`]), so it names the session the conversation is
/// on then.
struct InClaude {
    carried: Carried,
    claude: ClaudeNow,
}

/// The events Claude Code has: each speaks the dialect as of now.
macro_rules! in_claude {
    ($($payload:ident),+) => {$(
        #[async_trait]
        impl Hook<$payload> for InClaude {
            async fn run(
                &self,
                payload: $payload,
                cancel: &CancellationToken,
            ) -> Result<<$payload as Event>::Decision, BoxError> {
                let claude = (self.claude)();
                match &self.carried {
                    #[cfg(feature = "shell")]
                    Carried::Shell(hook) => hook.clone().with_dialect(claude).run(payload, cancel).await,
                    #[cfg(feature = "http")]
                    Carried::Http(hook) => hook.clone().with_dialect(claude).run(payload, cancel).await,
                }
            }
        }
    )+};
}
in_claude!(
    ToolBeforePayload,
    ToolAfterPayload,
    PromptSubmitPayload,
    SystemPromptPayload,
    TurnEndPayload
);

/// Claude Code has no `model.before` or `model.after`: a `claude` entry on
/// one is refused when read, so these never run; if one did, it fails.
macro_rules! no_claude_event {
    ($($payload:ident),+) => {$(
        #[async_trait]
        impl Hook<$payload> for InClaude {
            async fn run(
                &self,
                _: $payload,
                _: &CancellationToken,
            ) -> Result<<$payload as Event>::Decision, BoxError> {
                Err(format!("{} has no Claude Code event", <$payload as Event>::NAME).into())
            }
        }
    )+};
}
no_claude_event!(ModelBeforePayload, ModelAfterPayload);
