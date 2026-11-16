//! Glob: files by pattern, walked over the workspace `Fs` (`fs_walk.rs`).

use async_trait::async_trait;
use haest_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str};
use serde_json::{Value, json};

use crate::{
    canonical::pick,
    fs_walk::{WALK_LIMIT, glob_fs},
};

const GLOB_LIMIT: usize = 200;

/// The Glob tool (`Glob`, kind `file.glob`, read-only): files by glob pattern,
/// newest first.
///
/// Input: `pattern` (`src/**/*.ts`) and an optional `path` to search from
/// (default: the context's `cwd`). It returns absolute paths, at most 200 with
/// a count of the rest, and says so when the walk stopped at 100,000 entries.
/// node_modules and .git are skipped; a cancelled call returns what it found
/// so far.
///
/// The pattern language is the TS haest's (Bun's glob): `*` stops at `/`,
/// `**` crosses directories, `?`, `[...]`, `{a,b}`, dot files match, case
/// matters, and a leading `!` negates the whole pattern.
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }
    fn kind(&self) -> &str {
        "file.glob"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "Find files by glob pattern (e.g. \"src/**/*.ts\"). Returns paths sorted by modification time, newest first, capped at 200. Skips node_modules and .git.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern"},
                "path": {"type": "string", "description": "Directory to search from (default: working directory)"}
            },
            "required": ["pattern"]
        })
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        pick(input, &[("pattern", "pattern"), ("cwd", "path")])
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = req_str(&input, "pattern")?;
        let base = ctx.resolve(opt_str(&input, "path")?.unwrap_or("."));
        let fs = ctx.workspace.fs();
        let mut found = glob_fs(fs.as_ref(), &base, pattern, &ctx.cancel)
            .await
            .map_err(ToolError::Input)?;
        // Newest first; stable, so equal times keep walk order.
        found.hits.sort_by_key(|h| std::cmp::Reverse(h.mtime));
        let total = found.hits.len();
        let shown: Vec<String> = found
            .hits
            .iter()
            .take(GLOB_LIMIT)
            .map(|h| h.path.display().to_string())
            .collect();
        let more = if total > GLOB_LIMIT {
            format!("\n[{} more; narrow the pattern]", total - GLOB_LIMIT)
        } else {
            String::new()
        };
        let cap = if found.capped {
            format!("\n[stopped after visiting {WALK_LIMIT} entries; narrow the path or pattern]")
        } else {
            String::new()
        };
        Ok(ToolOutput::text(if shown.is_empty() {
            format!("No files found{cap}")
        } else {
            format!("{}{more}{cap}", shown.join("\n"))
        }))
    }
}
