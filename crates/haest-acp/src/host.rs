//! What the host hands haest-acp: how a conversation the client opens is
//! built ([`Conversations`]), and where the client opened it ([`Setup`]).

use std::path::PathBuf;

use async_trait::async_trait;
use haest_core::error::BoxError;
use haest_runtime::RuntimeInputs;

/// PLUG POINT: what a conversation an ACP client opens is built from.
///
/// haest-acp asks for the inputs each time the client opens a session
/// (`session/new`, `session/load`, `session/resume`) and when it lists the
/// saved ones (`session/list`, for the sessions dir in the settings). The
/// host answers for the directory the client named ([`Setup::cwd`]): its
/// settings (model, tools, workspace), keys, providers and MCP servers, as
/// it would for any conversation it runs there. The `haest` command loads
/// haest's own files for that directory.
///
/// To what the host returns, haest-acp adds the MCP servers the client
/// hands over (after the host's own) and its own plan mode
/// ([`RuntimeInputs::with_plan`]), whose listener tells the client the
/// session's mode: a plan mode the host set is replaced. An error refuses
/// the client's request, with the error's report as the reason.
///
/// ```
/// use async_trait::async_trait;
/// use haest_acp::{Conversations, Setup};
/// use haest_core::{error::BoxError, fs::MemFs};
/// use haest_runtime::{RuntimeInputs, World};
/// use haest_settings::Settings;
///
/// /// Every conversation over files of its own, keyed for OpenRouter.
/// struct InMemory;
///
/// #[async_trait]
/// impl Conversations for InMemory {
///     async fn inputs(&self, setup: &Setup) -> Result<RuntimeInputs, BoxError> {
///         let settings = Settings::bare(&setup.cwd, "/home/me");
///         let keys = |name: &str| (name == "OPENROUTER_API_KEY").then(|| "sk-or-example".to_string());
///         Ok(RuntimeInputs::new(settings, World::files(MemFs::new())).with_keys(keys))
///     }
/// }
/// ```
#[async_trait]
pub trait Conversations: Send + Sync {
    /// The inputs of a conversation in `setup.cwd`.
    async fn inputs(&self, setup: &Setup) -> Result<RuntimeInputs, BoxError>;
}

haest_core::plug_ref! {
    /// A [`Conversations`], shared.
    pub struct ConversationsRef(dyn Conversations);
}

/// Where the client wants a conversation: what [`Conversations::inputs`] is
/// asked for. More may come (the client's extra directories): a host reads
/// the fields it knows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Setup {
    /// The directory the client opened the session in (absolute: the
    /// protocol requires it), the conversation's `cwd`.
    pub cwd: PathBuf,
}

impl Setup {
    /// A conversation in `cwd`.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }
}
