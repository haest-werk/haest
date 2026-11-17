//! Write and Edit: the tools that change a file ([`WriteTool`], [`EditTool`];
//! Read lives in [`crate::read`]). Both go through the workspace's files, so in
//! the default sandbox an edit stays in memory until the human applies it.
//!
//! Names and parameter keys follow the Claude Code convention (`file_path`,
//! `old_string`, ...) on purpose: every frontier model has seen millions of
//! those calls, and hosts' legacy renderers already understand them.

use std::path::Path;

use async_trait::async_trait;
use haest_core::{
    fs::ensure_dir,
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_bool, req_str},
};
use serde_json::{Value, json};

use crate::{canonical::pick, read::within_cap};

/// A string input that may be empty (a file's content, a replacement).
fn any_str<'a>(input: &'a JsonMap, key: &str) -> Result<&'a str, ToolError> {
    match input.get(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(ToolError::Input(format!(
            "\"{key}\" is required and must be a string"
        ))),
    }
}

/// The Write tool (`Write`, kind `file.write`): creates a file, or replaces
/// one whole, making its parent directories.
///
/// Input: `file_path` (absolute, or relative to the context's `cwd`) and
/// `content` (may be empty). The model is told whether it created or
/// overwrote the file, and how many bytes it wrote.
pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "Write"
    }
    fn kind(&self) -> &str {
        "file.write"
    }
    fn description(&self) -> String {
        "Create or overwrite a file with the given content. Parent directories are created. Prefer Edit for changes to an existing file.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "Absolute path (or relative to the working directory)"},
                "content": {"type": "string", "description": "The full file content"}
            },
            "required": ["file_path", "content"]
        })
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        pick(input, &[("path", "file_path"), ("content", "content")])
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = ctx.resolve(req_str(&input, "file_path")?);
        let content = any_str(&input, "content")?;
        let fs = ctx.workspace().fs();
        let existed = fs.exists(&path).await;
        if let Some(parent) = path.parent() {
            ensure_dir(fs.as_ref(), parent).await?;
        }
        fs.write(&path, content.as_bytes()).await?;
        let verb = if existed { "Overwrote" } else { "Created" };
        Ok(ToolOutput::text(format!(
            "{verb} {} ({} bytes)",
            path.display(),
            content.len()
        )))
    }
}

/// The Edit tool (`Edit`, kind `file.edit`): replaces an exact string in an
/// existing file.
///
/// Input: `file_path`, `old_string` (must match byte for byte), `new_string`
/// (must differ from it, may be empty) and optional `replace_all`. The
/// replacement is literal: `$&` and friends mean nothing here.
///
/// Refused with a [`ToolError::Input`] that says what to do: no such file
/// (use Write), `old_string` not found, found more than once without
/// `replace_all`, identical strings, or a file over 32 MB.
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }
    fn kind(&self) -> &str {
        "file.edit"
    }
    fn description(&self) -> String {
        "Replace an exact string in a file. old_string must match the file byte-for-byte (including indentation) and be unique unless replace_all is true. Include enough surrounding lines to make it unique.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "Absolute path (or relative to the working directory)"},
                "old_string": {"type": "string", "description": "Exact text to replace"},
                "new_string": {"type": "string", "description": "Replacement text (must differ from old_string)"},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence (default false)"}
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        pick(
            input,
            &[
                ("path", "file_path"),
                ("oldText", "old_string"),
                ("newText", "new_string"),
                ("replaceAll", "replace_all"),
            ],
        )
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = ctx.resolve(req_str(&input, "file_path")?);
        let old = req_str(&input, "old_string")?;
        let new = any_str(&input, "new_string")?;
        if old == new {
            return Err(ToolError::Input(
                "old_string and new_string are identical; nothing to do".into(),
            ));
        }
        let fs = ctx.workspace().fs();
        let Ok(stat) = fs.stat(&path).await else {
            return Err(ToolError::Input(format!(
                "File does not exist: {}. Use Write to create it.",
                path.display()
            )));
        };
        within_cap(
            &stat,
            &path.display().to_string(),
            "change it with Bash (sed)",
        )?;
        let text = fs.read_to_string(&path).await?;
        let all = opt_bool(&input, "replace_all")?;
        let (replaced, count) = replace_checked(&text, old, new, all, &path)?;
        fs.write(&path, replaced.as_bytes()).await?;
        let s = if count == 1 { "" } else { "s" };
        Ok(ToolOutput::text(format!(
            "Edited {} ({count} replacement{s})",
            path.display()
        )))
    }
}

/// Apply the edit, refusing a missing or ambiguous match with a message the model
/// can act on. The replacement is literal: `$&` and friends mean nothing here.
fn replace_checked(
    text: &str,
    old: &str,
    new: &str,
    all: bool,
    path: &Path,
) -> Result<(String, usize), ToolError> {
    let count = text.matches(old).count();
    let shown = path.display();
    if count == 0 {
        return Err(ToolError::Input(format!(
            "old_string not found in {shown}. It must match exactly, including whitespace. Read the file again and copy the text."
        )));
    }
    if count > 1 && !all {
        return Err(ToolError::Input(format!(
            "old_string occurs {count} times in {shown}. Add surrounding context to make it unique, or set replace_all."
        )));
    }
    Ok(if all {
        (text.replace(old, new), count)
    } else {
        (text.replacen(old, new, 1), 1)
    })
}

#[cfg(test)]
mod tests;
