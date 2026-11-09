//! kern's built-in tools. PORT (T7): `tools/*.ts`.
//!
//! Every tool here touches files and commands ONLY through `ctx.workspace`
//! (`fs()`, `shell`, `run`), so the sandbox's in-memory files and the real disk
//! look the same to them. WebFetch is the one exception: it runs in the HOST,
//! never through the workspace, behind the net guard ([`net_guard`]).
//!
//! | Tool | Kind | Module |
//! |---|---|---|
//! | Read | `file.read` | [`read`] (PLUG POINT: [`read::ReadIntercept`]) |
//! | Write, Edit | `file.write`, `file.edit` | [`files`] |
//! | Glob, Grep | `file.glob`, `text.search` | [`search`] over [`fs_walk`] |
//! | Bash | `shell.exec` | [`bash`] |
//! | WebFetch | `web.fetch` | [`web_fetch`] (PLUG POINT: [`web_fetch::WebFetchNet`]) |

use std::sync::Arc;

use kern_core::tool::Tool;

pub mod bash;
mod canonical;
pub mod files;
pub mod fs_walk;
pub mod net_guard;
pub mod read;
pub mod search;
#[cfg(test)]
mod testkit;
pub mod web_fetch;

pub use bash::{BashOptions, BashTool};
pub use files::{EditTool, WriteTool};
pub use read::{ReadIntercept, ReadTool};
pub use search::{GlobTool, GrepTool};
pub use web_fetch::{WebFetchNet, WebFetchTool};

/// What a host can change about the built-in tools.
#[derive(Clone)]
pub struct BuiltinOptions {
    /// PLUG POINT: asked before the workspace fs on every Read, so a host can
    /// answer some paths itself (an `attachment:<id>` that is no path at all).
    pub read_intercept: Option<Arc<dyn ReadIntercept>>,
    pub bash: BashOptions,
    /// WebFetch's resolver and HTTP client; None = no WebFetch tool at all (a
    /// host with its own fetch, or no door to the network).
    pub web_fetch: Option<WebFetchNet>,
}

impl Default for BuiltinOptions {
    fn default() -> Self {
        Self {
            read_intercept: None,
            bash: BashOptions::default(),
            web_fetch: Some(WebFetchNet::default()),
        }
    }
}

/// The built-in tools, in the order the model sees them.
pub fn builtin_tools(opts: BuiltinOptions) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadTool::new(opts.read_intercept)),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(BashTool::new(opts.bash)),
        Arc::new(GrepTool),
        Arc::new(GlobTool),
    ];
    if let Some(net) = opts.web_fetch {
        tools.push(Arc::new(WebFetchTool::new(net)));
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
        tools.iter().map(|t| t.name().to_string()).collect()
    }

    #[test]
    fn the_builtin_list_has_every_tool_and_web_fetch_is_optional() {
        let all = builtin_tools(BuiltinOptions::default());
        assert_eq!(
            names(&all),
            ["Read", "Write", "Edit", "Bash", "Grep", "Glob", "WebFetch"]
        );
        let read_only: Vec<_> = all
            .iter()
            .filter(|t| t.read_only())
            .map(|t| t.name())
            .collect();
        assert_eq!(read_only, ["Read", "Grep", "Glob", "WebFetch"]);
        let offline = builtin_tools(BuiltinOptions {
            web_fetch: None,
            ..Default::default()
        });
        assert!(!names(&offline).contains(&"WebFetch".to_string()));
    }

    #[test]
    fn every_tool_states_its_canonical_kind() {
        let kinds: Vec<_> = builtin_tools(BuiltinOptions::default())
            .iter()
            .map(|t| t.kind().to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "file.read",
                "file.write",
                "file.edit",
                "shell.exec",
                "text.search",
                "file.glob",
                "web.fetch"
            ]
        );
    }
}
