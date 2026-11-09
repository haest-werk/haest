//! Write / Edit (Read lives in `read.rs`). Names and parameter keys follow the
//! Claude Code convention (`file_path`, `old_string`, ...) on purpose: every
//! frontier model has seen millions of those calls, and hosts' legacy renderers
//! already understand them.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use kern_core::{
    fs::{Fs, ensure_dir},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_bool, req_str},
};
use serde_json::{Value, json};

use crate::canonical::pick;

/// A string input that may be empty (a file's content, a replacement).
fn any_str<'a>(input: &'a JsonMap, key: &str) -> Result<&'a str, ToolError> {
    match input.get(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(ToolError::Input(format!(
            "\"{key}\" is required and must be a string"
        ))),
    }
}

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
        let fs = ctx.workspace.fs();
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
        let fs = ctx.workspace.fs();
        let edit = read_edit_input(fs.as_ref(), &input, &ctx.cwd).await?;
        let text = fs.read_to_string(&edit.path).await?;
        let (replaced, count) = replace_checked(&text, &edit)?;
        fs.write(&edit.path, replaced.as_bytes()).await?;
        let s = if count == 1 { "" } else { "s" };
        Ok(ToolOutput::text(format!(
            "Edited {} ({count} replacement{s})",
            edit.path.display()
        )))
    }
}

struct EditInput<'a> {
    path: PathBuf,
    old: &'a str,
    new: &'a str,
    all: bool,
}

async fn read_edit_input<'a>(
    fs: &dyn Fs,
    input: &'a JsonMap,
    cwd: &Path,
) -> Result<EditInput<'a>, ToolError> {
    let path = kern_core::tool::resolve_path(cwd, req_str(input, "file_path")?);
    let old = req_str(input, "old_string")?;
    let new = any_str(input, "new_string")?;
    if old == new {
        return Err(ToolError::Input(
            "old_string and new_string are identical; nothing to do".into(),
        ));
    }
    if !fs.exists(&path).await {
        return Err(ToolError::Input(format!(
            "File does not exist: {}. Use Write to create it.",
            path.display()
        )));
    }
    Ok(EditInput {
        path,
        old,
        new,
        all: opt_bool(input, "replace_all"),
    })
}

/// Apply the edit, refusing a missing or ambiguous match with a message the model
/// can act on. The replacement is literal: `$&` and friends mean nothing here.
fn replace_checked(text: &str, e: &EditInput) -> Result<(String, usize), ToolError> {
    let count = text.matches(e.old).count();
    let shown = e.path.display();
    if count == 0 {
        return Err(ToolError::Input(format!(
            "old_string not found in {shown}. It must match exactly, including whitespace. Read the file again and copy the text."
        )));
    }
    if count > 1 && !e.all {
        return Err(ToolError::Input(format!(
            "old_string occurs {count} times in {shown}. Add surrounding context to make it unique, or set replace_all."
        )));
    }
    if e.all {
        return Ok((text.replace(e.old, e.new), count));
    }
    Ok((text.replacen(e.old, e.new, 1), 1))
}

#[cfg(test)]
mod tests;
