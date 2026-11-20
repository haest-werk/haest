//! Write and Edit: the tools that change a file ([`WriteTool`], [`EditTool`];
//! Read lives in [`crate::read`]). Both go through the workspace's files, so in
//! the default sandbox an edit stays in memory until the human applies it.
//!
//! Names and parameter keys follow the Claude Code convention (`file_path`,
//! `old_string`, ...) on purpose: every frontier model has seen millions of
//! those calls, and hosts' legacy renderers already understand them.

use std::{io, path::Path};

use async_trait::async_trait;
use haest_core::{
    fs::{Fs, ensure_dir},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_bool, req_str},
};
use serde_json::{Value, json};

use crate::{canonical::pick, read::within_cap};

/// Put `data` in the file at `path`, making its parent directories. One
/// that is there is replaced whole or not at all ([`Fs::write_whole`]): a
/// disk that fills up partway leaves the human's version as it was, which
/// the model may not hold. A new one is made with [`Fs::write`]. Returns
/// whether one was there.
pub(crate) async fn save(fs: &dyn Fs, path: &Path, data: &[u8]) -> io::Result<bool> {
    if fs.exists(path).await {
        fs.write_whole(path, data).await?;
        return Ok(true);
    }
    if let Some(parent) = path.parent() {
        ensure_dir(fs, parent).await?;
    }
    fs.write(path, data).await?;
    Ok(false)
}

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
///
/// A file that is there is replaced as [`EditTool`] replaces it, whole or
/// not at all ([`Fs::write_whole`]), so the human's version outlives a
/// disk that fills up partway; a new file is made with [`Fs::write`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct WriteTool;

impl WriteTool {
    /// The Write tool.
    pub fn new() -> Self {
        Self
    }
}

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
        let existed = save(fs.as_ref(), &path, content.as_bytes()).await?;
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
///
/// The edited text replaces the file whole or not at all
/// ([`Fs::write_whole`]): the model holds only what it replaced, so it
/// could never put back a file the disk cut short. In the real workspace
/// (haest-workspace's `RealWorkspace`), the text goes into a new file
/// beside the file, which takes its permission bits and is renamed over
/// it: a disk that fills up, or a kill, partway leaves the file as it
/// was, and the tool fails with the disk's error. On a disk that reads two
/// spellings of a name as one, the system decides which one the file goes
/// by afterwards, as for any program that saves by renaming. The new file
/// does not keep the old one's extended attributes or ACLs, and whatever holds
/// the old one keeps it: a process that has it open (a log a server
/// appends to, `tail -f`), an inotify watch on it, a container it is
/// bind-mounted into on its own. The disk needs room for the second copy,
/// and a kill partway can leave a `.haest-*` file beside it. A file with
/// another hard link, another owner or group, or a symlink, and one where
/// the disk refuses to make the new file or rename it, is written in
/// place, where a full disk still cuts it short and the tool says so. In the default sandbox the edit stays in memory
/// until the human applies it, which writes the same way.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct EditTool;

impl EditTool {
    /// The Edit tool.
    pub fn new() -> Self {
        Self
    }
}

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
        // Whole or not at all: the model holds only what it replaced, so a
        // file a full disk cut short is one it cannot put back.
        fs.write_whole(&path, replaced.as_bytes()).await?;
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
