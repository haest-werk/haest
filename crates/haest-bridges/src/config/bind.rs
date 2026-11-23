//! The `hooks` setting bound: each entry a [`ShellHook`] or an [`HttpHook`]
//! in a [`Binding`] with its matcher, timeout and `on_error`, all in one
//! [`Hooks`], as a Rust host binds them by hand.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use haest_core::{ChildEnv, error::BoxError};
#[cfg(feature = "http")]
use haest_http::HttpClientRef;
use haest_mods::{
    Binding, Event, Hook, HookDialect, HookEntry, HookSettings, Hooks, ModEvent, ModelAfterPayload,
    ModelBeforePayload, PromptSubmitPayload, SystemPromptPayload, ToolAfterPayload,
    ToolBeforePayload, ToolEvent, TurnEndPayload,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "http")]
use crate::HttpHook;
#[cfg(feature = "shell")]
use crate::ShellHook;
use crate::{
    Claude, Haest,
    dialect::{Dialect, HookFailure, Reply, Vars},
};

/// What the [`Hooks`] are named in a report (`Mod [hooks] failed in
/// tool.before: hook 2 on tool.before failed: ..`): the setting they come
/// from.
const NAME: &str = "[hooks]";

/// What a host hands the hooks its `hooks` setting binds
/// ([`bind`](Self::bind)): the conversation's directory, the variables a
/// command gets (or that no process may start), and the HTTP client a POST
/// goes through. The session the `claude` dialect names is not handed here:
/// each event brings the one the conversation's
/// [`ModEnv`](haest_mods::ModEnv) names then
/// ([`set_session`](haest_mods::ModEnv::set_session)), so a hook bound once
/// names the new session after the human's /clear.
///
/// ```
/// use haest_bridges::HookHost;
/// use haest_core::ChildEnv;
///
/// let path = std::env::var("PATH").unwrap_or_default();
/// let host = HookHost::new("/work/project").with_env(ChildEnv::new([("PATH", path)]));
/// ```
#[derive(Clone)]
pub struct HookHost {
    cwd: PathBuf,
    env: Option<ChildEnv>,
    #[cfg(feature = "http")]
    client: HttpClientRef,
}

impl HookHost {
    /// Hooks of a conversation in `cwd`: a command runs there, and the
    /// `claude` dialect names it as `cwd`. No process may start until
    /// [`with_env`](Self::with_env); a POST goes through haest-http's
    /// default client.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            env: None,
            #[cfg(feature = "http")]
            client: HttpClientRef::default(),
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
}

impl fmt::Debug for HookHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookHost")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

impl HookHost {
    /// Every entry of `settings` bound in one [`Hooks`], named `[hooks]` in
    /// reports: a `command` as a [`ShellHook`], a `url` as an [`HttpHook`],
    /// each in the [`Binding`] of its event with its matcher, timeout and
    /// `on_error`, in the dialect it names. An event's entries bind in the
    /// order written, so a report's `hook 2 on tool.before` is the second
    /// `[[hooks."tool.before"]]` across the config's layers. None when no
    /// entry binds.
    ///
    /// An entry that cannot bind here is left out with a note, the second
    /// value: a command where no process may start
    /// ([`with_env`](Self::with_env)), a command without this crate's
    /// `shell` feature, a URL without `http`.
    pub fn bind(&self, settings: &HookSettings) -> (Option<Hooks>, Vec<String>) {
        let mut hooks = Hooks::new().with_name(NAME);
        let (mut bound, mut notes) = (0, Vec::new());
        for (event, name, entry) in settings.named_entries() {
            match bind(hooks.clone(), event, entry, self) {
                Ok(more) => {
                    hooks = more;
                    bound += 1;
                }
                Err(why) => notes.push(format!("{name} is not bound: {why}")),
            }
        }
        ((bound > 0).then_some(hooks), notes)
    }
}

/// `entry` bound to `event` in `hooks`, or why it cannot be here. An event
/// Claude Code has takes either dialect; the others take haest's only, and
/// the build holds each arm to haest-mods' `ModEvent::claude_code`.
fn bind(
    hooks: Hooks,
    event: ModEvent,
    entry: &HookEntry,
    host: &HookHost,
) -> Result<Hooks, String> {
    let carried = carried(entry, host)?;
    let named = Named::of(entry.dialect());
    Ok(match event {
        ModEvent::ToolBefore => hooks.on_tool_before(tool_binding::<ToolBeforePayload>(
            carried.speaking(named),
            entry,
        )),
        ModEvent::ToolAfter => hooks.on_tool_after(tool_binding::<ToolAfterPayload>(
            carried.speaking(named),
            entry,
        )),
        ModEvent::PromptSubmit => hooks.on_prompt_submit(binding::<PromptSubmitPayload>(
            carried.speaking(named),
            entry,
        )),
        ModEvent::SystemPrompt => hooks.on_system_prompt(binding::<SystemPromptPayload>(
            carried.speaking(named),
            entry,
        )),
        ModEvent::ModelBefore => hooks.on_model_before(binding::<ModelBeforePayload>(
            haest_only::<ModelBeforePayload>(carried, named)?,
            entry,
        )),
        ModEvent::ModelAfter => hooks.on_model_after(binding::<ModelAfterPayload>(
            haest_only::<ModelAfterPayload>(carried, named)?,
            entry,
        )),
        ModEvent::TurnEnd => {
            hooks.on_turn_end(binding::<TurnEndPayload>(carried.speaking(named), entry))
        }
        // An event newer than this match: the tests fail until it is here.
        other => return Err(format!("{other} can't be bound from the config yet")),
    })
}

/// `carried` on an event Claude Code does not have, which only haest's
/// dialect speaks. The `hooks` setting refuses a `claude` entry on one when
/// it is read; this is that refusal, should one get here. It does not build
/// for an event haest-mods gives a Claude Code name: that one binds in
/// either dialect.
fn haest_only<E: Event>(carried: Carried<Haest>, named: Named) -> Result<Carried<Haest>, String> {
    const {
        assert!(
            E::NAME.claude_code().is_none(),
            "Claude Code has this event: bind it in the dialect its entry names"
        );
    };
    match named {
        Named::Haest => Ok(carried),
        Named::Claude => Err(format!("{} has no Claude Code event", E::NAME)),
    }
}

/// The hook that carries `entry`'s event, in haest's dialect.
fn carried(entry: &HookEntry, host: &HookHost) -> Result<Carried<Haest>, String> {
    match (entry.command(), entry.url()) {
        (Some(command), _) => command_hook(command, host),
        (None, Some(url)) => url_hook(url, entry, host),
        // An entry is a command or a URL: haest-mods builds no other.
        (None, None) => Err("it is neither a command nor a URL".into()),
    }
}

/// `command` as a [`ShellHook`] of `host`'s.
#[cfg(feature = "shell")]
fn command_hook(command: &str, host: &HookHost) -> Result<Carried<Haest>, String> {
    let Some(env) = &host.env else {
        return Err(
            "a command hook needs a host that starts processes, and this one starts none".into(),
        );
    };
    let hook = ShellHook::new(command)
        .with_cwd(&host.cwd)
        .with_env(env.clone());
    Ok(Carried::Shell(hook))
}

#[cfg(not(feature = "shell"))]
fn command_hook(_: &str, _: &HookHost) -> Result<Carried<Haest>, String> {
    Err("a command hook needs haest-bridges' `shell` feature".into())
}

/// `url` as an [`HttpHook`] through `host`'s client, with `entry`'s headers.
#[cfg(feature = "http")]
fn url_hook(url: &str, entry: &HookEntry, host: &HookHost) -> Result<Carried<Haest>, String> {
    let hook = HttpHook::new(url)
        .with_client(host.client.clone())
        .with_cwd(&host.cwd);
    let hook = (entry.headers()).fold(hook, |h, (name, value)| h.with_header(name, value));
    Ok(Carried::Http(hook))
}

#[cfg(not(feature = "http"))]
fn url_hook(_: &str, _: &HookEntry, _: &HookHost) -> Result<Carried<Haest>, String> {
    Err("an HTTP hook needs haest-bridges' `http` feature".into())
}

/// `hook` with `entry`'s timeout and `on_error`, else its event's defaults.
fn binding<E: Event>(hook: impl Hook<E>, entry: &HookEntry) -> Binding<E> {
    let mut binding = Binding::new(hook);
    if let Some(timeout) = entry.timeout() {
        binding = binding.with_timeout(timeout);
    }
    if let Some(on_error) = entry.on_error() {
        binding = binding.with_on_error(on_error);
    }
    binding
}

/// [`binding`], and the tools `entry`'s matcher picks.
fn tool_binding<E: ToolEvent>(hook: impl Hook<E>, entry: &HookEntry) -> Binding<E> {
    let binding = binding(hook, entry);
    match entry.matcher() {
        Some(pattern) => binding.with_matcher(pattern),
        None => binding,
    }
}

/// A command or a POST, speaking `D`: a hook of every event `D` speaks.
enum Carried<D> {
    #[cfg(feature = "shell")]
    Shell(ShellHook<D>),
    #[cfg(feature = "http")]
    Http(HttpHook<D>),
}

impl Carried<Haest> {
    /// The same command or POST, speaking `named`: a hook of the events
    /// both dialects speak.
    fn speaking(self, named: Named) -> Carried<Named> {
        match self {
            #[cfg(feature = "shell")]
            Carried::Shell(hook) => Carried::Shell(hook.with_dialect(named)),
            #[cfg(feature = "http")]
            Carried::Http(hook) => Carried::Http(hook.with_dialect(named)),
        }
    }
}

#[async_trait]
impl<E: Event, D: Dialect<E>> Hook<E> for Carried<D> {
    async fn run(&self, payload: E, cancel: &CancellationToken) -> Result<E::Decision, BoxError> {
        match self {
            #[cfg(feature = "shell")]
            Carried::Shell(hook) => hook.run(payload, cancel).await,
            #[cfg(feature = "http")]
            Carried::Http(hook) => hook.run(payload, cancel).await,
        }
    }
}

/// The dialect an entry names, as a value: it speaks the events both
/// [`Haest`] and [`Claude`] do, the ones Claude Code has.
#[derive(Debug, Clone, Copy)]
enum Named {
    Haest,
    Claude,
}

impl Named {
    fn of(dialect: HookDialect) -> Self {
        match dialect {
            HookDialect::Claude => Self::Claude,
            // `haest`, and a dialect newer than this match: the event's own JSON.
            _ => Self::Haest,
        }
    }
}

impl Vars for Named {
    fn env(&self, dir: &Path) -> ChildEnv {
        match self {
            Self::Haest => Haest.env(dir),
            Self::Claude => Claude.env(dir),
        }
    }
}

impl<E: Event> Dialect<E> for Named
where
    Claude: Dialect<E>,
{
    fn input(&self, payload: &E, json: Value, dir: &Path) -> Result<Value, HookFailure> {
        match self {
            Self::Haest => Dialect::<E>::input(&Haest, payload, json, dir),
            Self::Claude => Dialect::<E>::input(&Claude, payload, json, dir),
        }
    }

    fn decide(&self, payload: E, reply: Reply) -> Result<E::Decision, HookFailure> {
        match self {
            Self::Haest => Dialect::<E>::decide(&Haest, payload, reply),
            Self::Claude => Dialect::<E>::decide(&Claude, payload, reply),
        }
    }
}
