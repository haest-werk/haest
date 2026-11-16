//! Grep (ripgrep) and Glob, the two search tools ([`GrepTool`], [`GlobTool`]),
//! both through the workspace.
//!
//! rg respects .gitignore natively and runs through
//! [`Workspace::run`](haest_core::workspace::Workspace::run); Glob walks the
//! workspace `Fs` ([`crate::fs_walk`]) and skips node_modules/.git explicitly.
//! The rg argv uses only flags the real rg and a sandbox's rg both accept.
//! Nothing the model sends can be read as a flag: the pattern sits behind `-e`,
//! `--glob`/`--type` take the next argument as their value, and a target that
//! starts with `-` gets `./`.

use std::time::Duration;

use async_trait::async_trait;
use haest_core::{
    tool::{
        JsonMap, TRUNCATE_CHARS, Tool, ToolContext, ToolError, ToolOutput, opt_bool, opt_num,
        opt_str, req_str, truncate_middle,
    },
    workspace::{EXIT_NOT_FOUND, ExecOptions},
};
use serde_json::{Value, json};

use crate::{
    canonical::pick,
    fs_walk::{WALK_LIMIT, glob_fs},
    read::clip_line,
};

const GLOB_LIMIT: usize = 200;
const GREP_TIMEOUT: Duration = Duration::from_secs(60);
/// A Grep match (or rg's error line) longer than this is cut: a minified file
/// would otherwise fill the answer with one line.
const GREP_MAX_LINE_CHARS: usize = 500;

/// The Glob tool (`Glob`, kind `file.glob`, read-only): files by glob pattern,
/// newest first.
///
/// Input: `pattern` (`src/**/*.ts`; the language is on [`crate::fs_walk`]) and
/// an optional `path` to search from (default: the context's `cwd`). It
/// returns absolute paths, at most 200 with a count of the rest, and says so
/// when the walk stopped at [`WALK_LIMIT`] entries. node_modules and .git are
/// skipped; a cancelled call returns what it found so far.
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

fn mode_flag(mode: &str) -> Option<&'static str> {
    match mode {
        "files_with_matches" => Some("-l"),
        "count" => Some("-c"),
        "content" => Some("-n"),
        _ => None,
    }
}

const BOOL_FLAGS: [(&str, &[&str]); 2] = [
    ("case_insensitive", &["-i"]),
    ("multiline", &["-U", "--multiline-dotall"]),
];
const VALUE_FLAGS: [(&str, &str); 2] = [("glob", "--glob"), ("type", "--type")];

/// The rg flags this call asks for (everything but the pattern and the target).
fn rg_args(input: &JsonMap) -> Result<Vec<String>, ToolError> {
    let mode = opt_str(input, "output_mode")?.unwrap_or("files_with_matches");
    let flag = mode_flag(mode).ok_or_else(|| {
        ToolError::Input("output_mode must be content, files_with_matches or count".into())
    })?;
    let mut args = vec![flag.to_string()];
    for (key, flags) in BOOL_FLAGS {
        if opt_bool(input, key) {
            args.extend(flags.iter().map(|f| f.to_string()));
        }
    }
    for (key, flag) in VALUE_FLAGS {
        if let Some(v) = opt_str(input, key)? {
            args.extend([flag.to_string(), v.to_string()]);
        }
    }
    if let Some(context) = opt_num(input, "context")?
        && mode == "content"
        && context != 0.0
    {
        args.extend(["-C".to_string(), (context.floor() as i64).to_string()]);
    }
    Ok(args)
}

/// The search target as an argument rg can only read as a path. rg reads one that
/// starts with `-` as a flag (`--pre=sh` runs every file under cwd as a script, and
/// Grep is read-only, so no approval stands in the way) and a bare `-` as stdin,
/// after `--` too; `./` in front names the same file.
fn target_arg(target: &str) -> String {
    if target.starts_with('-') {
        format!("./{target}")
    } else {
        target.to_string()
    }
}

/// The Grep tool (`Grep`, kind `text.search`, read-only): ripgrep over the
/// workspace's files.
///
/// It runs `rg` through the workspace ([`Workspace::run`](haest_core::workspace::Workspace::run)),
/// in the context's `cwd`, with a 60 s timeout: the real disk needs an `rg` on
/// the host's `PATH`, the sandbox has its own. With no `rg` (exit 127) the
/// model is told to use Bash with grep instead.
///
/// Input: `pattern` (a regex), and optional `path`, `glob`, `type`,
/// `output_mode` (`files_with_matches`, the default; `content` with line
/// numbers; `count`), `case_insensitive`, `context` (lines, content mode),
/// `multiline` and `head_limit` (lines of output). Lines over 500 characters
/// are cut, and the whole answer is kept to haest-core's [`TRUNCATE_CHARS`].
/// Files rg could not
/// read are named in a closing note; a search with no match says so.
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }
    fn kind(&self) -> &str {
        "text.search"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "Search file contents with ripgrep (regex). output_mode: \"files_with_matches\" (default), \"content\" (matching lines with line numbers), or \"count\". Filter with glob (e.g. \"*.ts\") or type (e.g. \"py\").".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regular expression (ripgrep syntax)"},
                "path": {"type": "string", "description": "File or directory to search (default: working directory)"},
                "glob": {"type": "string", "description": "Only search files matching this glob"},
                "type": {"type": "string", "description": "ripgrep file type, e.g. ts, py, rust"},
                "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                "case_insensitive": {"type": "boolean"},
                "context": {"type": "number", "description": "Lines of context around matches (content mode)"},
                "multiline": {"type": "boolean", "description": "Let patterns span lines"},
                "head_limit": {"type": "number", "description": "Return only the first N lines of output"}
            },
            "required": ["pattern"]
        })
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        pick(
            input,
            &[
                ("pattern", "pattern"),
                ("path", "path"),
                ("glob", "glob"),
                ("caseInsensitive", "case_insensitive"),
                ("contextLines", "context"),
                ("outputMode", "output_mode"),
                ("headLimit", "head_limit"),
            ],
        )
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = req_str(&input, "pattern")?;
        // Passed as given: rg resolves a relative target against cwd and then prints
        // cwd-relative paths, which is what the model should see.
        let target = opt_str(&input, "path")?.unwrap_or(".");
        let head = opt_num(&input, "head_limit")?;
        let mut argv = vec!["rg".to_string()];
        argv.extend(rg_args(&input)?);
        argv.extend(["-e".into(), pattern.into(), target_arg(target)]);
        let opts = ExecOptions {
            cwd: ctx.cwd.clone(),
            cancel: ctx.cancel.clone(),
            timeout: GREP_TIMEOUT,
        };
        let r = ctx.workspace.run(&argv, opts).await;
        if r.exit_code == EXIT_NOT_FOUND {
            return Err(ToolError::Input(
                "ripgrep (rg) is not installed on this host; use Bash with grep instead".into(),
            ));
        }
        let stderr = r.stderr.trim();
        // Exit 1 is "no match" -- unless rg also complained (a sandbox's rg exits 1 on a bad flag).
        if r.exit_code == 1 && stderr.is_empty() {
            return Ok(ToolOutput::text("No matches found"));
        }
        // Exit 2 with output: rg searched what it could and says why the rest failed.
        let partial = r.exit_code == 2 && !r.stdout.trim().is_empty();
        if r.exit_code != 0 && !partial {
            let why = if stderr.is_empty() {
                format!("exit {}", r.exit_code)
            } else {
                stderr.to_string()
            };
            return Err(ToolError::Input(format!("ripgrep failed: {why}")));
        }
        let mut text = format_matches(&r.stdout, head);
        if partial {
            text.push_str(&unreadable_note(stderr));
        }
        Ok(ToolOutput::text(text))
    }
}

/// One line on what rg could not read: its first complaint and how many more.
fn unreadable_note(stderr: &str) -> String {
    let mut lines = stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().map(|l| clip_line(l, GREP_MAX_LINE_CHARS));
    let more = match lines.count() {
        0 => String::new(),
        n => format!(" (+{n} more)"),
    };
    format!(
        "\n[some files could not be read: {}{more}]",
        first.unwrap_or_default()
    )
}

/// Long lines clipped, `head_limit` applied, the whole thing bounded.
fn format_matches(stdout: &str, head: Option<f64>) -> String {
    let all: Vec<_> = stdout
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(|l| clip_line(l, GREP_MAX_LINE_CHARS))
        .collect();
    let keep = match head {
        Some(h) if h > 0.0 => (h.floor() as usize).min(all.len()),
        _ => all.len(),
    };
    let more = if keep < all.len() {
        format!("\n[{} more lines]", all.len() - keep)
    } else {
        String::new()
    };
    truncate_middle(&(all[..keep].join("\n") + &more), TRUNCATE_CHARS).0
}

#[cfg(test)]
mod tests;
