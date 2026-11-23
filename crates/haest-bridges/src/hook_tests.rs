//! The kit for the hooks in another process, and the tests every event
//! passes through each of them: a [`Remote`] is a hook script or a test
//! server that writes down every input it gets and answers once, and the
//! tests drive haest-mods' events through it as through a `Hooks` closure.

use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(crate) use haest_mods::testing::*;
use haest_mods::{
    Binding, Hook, Hooks, ModEvent, ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload,
    SystemPromptPayload, ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
};
use serde_json::Value;

use crate::Claude;

mod claude;
mod every;
#[cfg(feature = "http")]
mod server;

#[cfg(feature = "http")]
pub(crate) use server::{Respond, Server};

/// A hook bound to `event`, in [`Hooks`] of its own. Every event of
/// haest-mods is here: a new one panics, so the tests that drive every
/// event through a shell and an HTTP hook fail until it is added.
pub(crate) fn bound<H>(event: ModEvent, hook: H) -> Hooks
where
    H: Hook<ToolBeforePayload>
        + Hook<ToolAfterPayload>
        + Hook<PromptSubmitPayload>
        + Hook<SystemPromptPayload>
        + Hook<ModelBeforePayload>
        + Hook<ModelAfterPayload>
        + Hook<TurnEndPayload>,
{
    let hooks = Hooks::new();
    match event {
        ModEvent::ToolBefore => hooks.on_tool_before(Binding::<ToolBeforePayload>::new(hook)),
        ModEvent::ToolAfter => hooks.on_tool_after(Binding::<ToolAfterPayload>::new(hook)),
        ModEvent::PromptSubmit => hooks.on_prompt_submit(Binding::<PromptSubmitPayload>::new(hook)),
        ModEvent::SystemPrompt => hooks.on_system_prompt(Binding::<SystemPromptPayload>::new(hook)),
        ModEvent::ModelBefore => hooks.on_model_before(Binding::<ModelBeforePayload>::new(hook)),
        ModEvent::ModelAfter => hooks.on_model_after(Binding::<ModelAfterPayload>::new(hook)),
        ModEvent::TurnEnd => hooks.on_turn_end(Binding::<TurnEndPayload>::new(hook)),
        other => panic!("{other} has no shell and HTTP coverage: bind it here"),
    }
}

/// The Claude Code event `event` is, or none: a new event panics, so its
/// mapping (or that it has none) is decided before the tests pass.
pub(crate) fn claude_event(event: ModEvent) -> Option<&'static str> {
    match event {
        ModEvent::ToolBefore => Some("PreToolUse"),
        ModEvent::ToolAfter => Some("PostToolUse"),
        ModEvent::PromptSubmit => Some("UserPromptSubmit"),
        ModEvent::SystemPrompt => Some("SessionStart"),
        ModEvent::TurnEnd => Some("Stop"),
        ModEvent::ModelBefore | ModEvent::ModelAfter => None,
        other => panic!("{other}: decide its Claude Code event, or that it has none"),
    }
}

/// A hook in the Claude dialect bound to `event`, which Claude Code has.
pub(crate) fn bound_claude<H>(event: ModEvent, hook: H) -> Hooks
where
    H: Hook<ToolBeforePayload>
        + Hook<ToolAfterPayload>
        + Hook<PromptSubmitPayload>
        + Hook<SystemPromptPayload>
        + Hook<TurnEndPayload>,
{
    let hooks = Hooks::new();
    match event {
        ModEvent::ToolBefore => hooks.on_tool_before(Binding::<ToolBeforePayload>::new(hook)),
        ModEvent::ToolAfter => hooks.on_tool_after(Binding::<ToolAfterPayload>::new(hook)),
        ModEvent::PromptSubmit => hooks.on_prompt_submit(Binding::<PromptSubmitPayload>::new(hook)),
        ModEvent::SystemPrompt => hooks.on_system_prompt(Binding::<SystemPromptPayload>::new(hook)),
        ModEvent::TurnEnd => hooks.on_turn_end(Binding::<TurnEndPayload>::new(hook)),
        other => panic!("{other} has no Claude Code event"),
    }
}

/// A transport a hook in another process is carried over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Via {
    /// A command: [`ShellHook`](crate::ShellHook).
    #[cfg(feature = "shell")]
    Shell,
    /// A POST: [`HttpHook`](crate::HttpHook).
    #[cfg(feature = "http")]
    Http,
}

/// Every transport this build has.
pub(crate) const VIAS: &[Via] = &[
    #[cfg(feature = "shell")]
    Via::Shell,
    #[cfg(feature = "http")]
    Via::Http,
];

/// A hook in another process that writes down every input it gets and
/// answers one text the first time, nothing after: a script run by a
/// [`ShellHook`](crate::ShellHook), or a [`Server`] behind an
/// [`HttpHook`](crate::HttpHook). Either way it runs in (and names as `cwd`)
/// a directory of its own.
pub(crate) struct Remote {
    via: Via,
    dir: tempfile::TempDir,
    #[cfg(feature = "http")]
    server: Option<Server>,
}

/// The script a shell [`Remote`] runs: its input appended to
/// `received.jsonl` (one line, as the bridges write JSON), `answer` printed
/// the first time.
#[cfg(feature = "shell")]
const ANSWER_ONCE_SH: &str = r#"cat >> received.jsonl
echo >> received.jsonl
if [ ! -e answered ]; then : > answered; cat answer; fi
"#;

impl Remote {
    /// A hook over `via` that answers `answer` the first time.
    #[cfg_attr(
        not(feature = "http"),
        expect(clippy::unused_async, reason = "the HTTP one starts its server")
    )]
    pub(crate) async fn answering(via: Via, answer: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        match via {
            #[cfg(feature = "shell")]
            Via::Shell => {
                std::fs::write(dir.path().join("hook.sh"), ANSWER_ONCE_SH).unwrap();
                std::fs::write(dir.path().join("answer"), answer).unwrap();
                Self {
                    via,
                    dir,
                    #[cfg(feature = "http")]
                    server: None,
                }
            }
            #[cfg(feature = "http")]
            Via::Http => Self {
                via,
                dir,
                server: Some(Server::start([Respond::With(200, answer.into())]).await),
            },
        }
    }

    /// The directory it runs in.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Every input it got, in order.
    pub(crate) fn received(&self) -> Vec<Value> {
        match self.via {
            #[cfg(feature = "shell")]
            Via::Shell => std::fs::read_to_string(self.dir().join("received.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).expect("the input is one line of JSON"))
                .collect(),
            #[cfg(feature = "http")]
            Via::Http => self
                .server()
                .seen()
                .iter()
                .map(|seen| serde_json::from_slice(&seen.body).expect("the body is JSON"))
                .collect(),
        }
    }

    #[cfg(feature = "http")]
    fn server(&self) -> &Server {
        self.server.as_ref().expect("an HTTP remote has a server")
    }

    /// It, in haest's dialect, bound to `event`.
    pub(crate) fn hooks(&self, event: ModEvent) -> Hooks {
        match self.via {
            #[cfg(feature = "shell")]
            Via::Shell => bound(event, self.shell()),
            #[cfg(feature = "http")]
            Via::Http => bound(event, self.http()),
        }
    }

    /// It, speaking `claude`, bound to `event`.
    pub(crate) fn claude_hooks(&self, event: ModEvent, claude: Claude) -> Hooks {
        match self.via {
            #[cfg(feature = "shell")]
            Via::Shell => bound_claude(event, self.shell().with_dialect(claude)),
            #[cfg(feature = "http")]
            Via::Http => bound_claude(event, self.http().with_dialect(claude)),
        }
    }

    #[cfg(feature = "shell")]
    fn shell(&self) -> crate::ShellHook {
        crate::ShellHook::new("sh hook.sh").with_cwd(self.dir())
    }

    #[cfg(feature = "http")]
    fn http(&self) -> crate::HttpHook {
        crate::HttpHook::new(self.server().url()).with_cwd(self.dir())
    }
}

/// Wait up to five seconds for `done`, checking every 20 ms.
pub(crate) async fn eventually(what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{what}: still not, after 5 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
