//! Read. Same name and keys as Claude Code's (`file_path`, `offset`, `limit`,
//! `allow_large`) on purpose -- every frontier model has seen millions of those calls.
//!
//! READ SIZES ITSELF. It used to go through the generic spill, which saved a big
//! read to a file and told the model to Read THAT in pieces: a Read answered with
//! "go Read a copy of what you just read". Now it returns as many whole lines as
//! fit its budget and says where to continue ([`text_output`]):
//!   - default budget 25,000 tokens, Claude Code's own Read limit;
//!   - `allow_large: true` lifts that (and the 2,000-line default) up to the room
//!     the context has left, so "I need the whole file" is one call, not ten;
//!   - neither ever exceeds that room, so a read cannot push the next model call
//!     over the request cap.
//!
//! PLUG POINT: [`ReadIntercept`] -- a host answers some paths itself before the
//! workspace fs is asked.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use kern_core::{
    fs::looks_binary,
    media::{media_from_bytes, media_type_from_name},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str},
};
use serde_json::{Value, json};

use crate::canonical::pick;

mod paging;

pub use paging::{READ_TOKEN_BUDGET, text_output};

/// A path the host answers itself before the workspace fs is asked: media that
/// must go through the host's own caps, an `attachment:<id>` that is no path at
/// all, a document converted to Markdown (page it with [`text_output`]).
#[async_trait]
pub trait ReadIntercept: Send + Sync {
    /// `raw_path` is `file_path` as the model sent it, unresolved. None = read it
    /// from the workspace fs as usual.
    async fn intercept(
        &self,
        raw_path: &str,
        input: &JsonMap,
        ctx: &ToolContext,
    ) -> Option<Result<ToolOutput, ToolError>>;
}

/// An image / PDF / audio / video (a file Read, a URL WebFetch) goes to the model
/// as media, and to the transcript as a URL when the host can publish it.
pub async fn media_output(
    label: &str,
    bytes: Vec<u8>,
    media_type: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let name = Path::new(label)
        .file_name()
        .map_or_else(|| label.to_string(), |n| n.to_string_lossy().into_owned());
    let part = media_from_bytes(&bytes, media_type, &name).map_err(ToolError::Input)?;
    let len = bytes.len();
    let kind = serde_json::to_value(part.kind).unwrap_or(Value::Null);
    let kind = kind.as_str().unwrap_or("media").to_string();
    let mut result = json!({"kind": "file", "mediaType": media_type, "bytes": len});
    if let Some(url) = ctx.host.publish(bytes, media_type).await {
        result["url"] = Value::String(url);
    }
    Ok(ToolOutput {
        content: format!("{label}: {kind} ({media_type}, {len} bytes), attached."),
        media: vec![part],
        result: Some(result),
        ..Default::default()
    })
}

/// The Read tool, optionally with a host's [`ReadIntercept`] in front of the fs.
#[derive(Default, Clone)]
pub struct ReadTool {
    intercept: Option<Arc<dyn ReadIntercept>>,
}

impl ReadTool {
    pub fn new(intercept: Option<Arc<dyn ReadIntercept>>) -> Self {
        Self { intercept }
    }

    /// Read from the workspace fs: the tool without its intercept.
    async fn read_fs(&self, input: &JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let fs = ctx.workspace.fs();
        let path = ctx.resolve(req_str(input, "file_path")?);
        let shown = path.display().to_string();
        if !fs.exists(&path).await {
            return Err(ToolError::Input(format!("File does not exist: {shown}")));
        }
        if fs.stat(&path).await?.is_dir() {
            return Err(ToolError::Input(format!(
                "{shown} is a directory; use Glob or Bash ls"
            )));
        }
        let buf = fs.read(&path).await?;
        if let Some(media_type) = media_type_from_name(&shown) {
            return media_output(&shown, buf, media_type, ctx).await;
        }
        if looks_binary(&buf) {
            return Err(ToolError::Input(format!(
                "{shown} is a binary file ({} bytes)",
                buf.len()
            )));
        }
        let text = String::from_utf8_lossy(&buf);
        // A decoder drops the byte-order mark; so does Read.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        text_output(text, buf.len(), input, ctx, "")
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }
    fn kind(&self) -> &str {
        "file.read"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "Read a file. Text comes back as lines prefixed with their 1-based line number and a tab; reads up to 2000 lines or ~25,000 tokens by default, use offset (1-based start line) and limit for larger files. Set allow_large only when you need the whole file in one call and the context has room. An image (png/jpg/gif/webp), PDF, audio or video file is attached so you see or hear it directly, when the current model accepts that kind of input.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "Absolute path (or relative to the working directory)"},
                "offset": {"type": "number", "description": "1-based line to start from"},
                "limit": {"type": "number", "description": "Number of lines to read"},
                "allow_large": {
                    "type": "boolean",
                    "description": "Read past the 25,000-token default in one call, up to the room the context has left"
                }
            },
            "required": ["file_path"]
        })
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        pick(
            input,
            &[
                ("path", "file_path"),
                ("offset", "offset"),
                ("limit", "limit"),
            ],
        )
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = req_str(&input, "file_path")?;
        if let Some(intercept) = &self.intercept
            && let Some(answer) = intercept.intercept(raw, &input, ctx).await
        {
            return answer;
        }
        self.read_fs(&input, ctx).await
    }
}

#[cfg(test)]
mod tests;
