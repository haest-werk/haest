//! Read: a file into the model's context. Start from [`ReadTool`].
//!
//! Same name and keys as Claude Code's (`file_path`, `offset`, `limit`,
//! `allow_large`) on purpose -- every frontier model has seen millions of those
//! calls. Text comes back as numbered lines (`  12\tthe line`); an image, PDF,
//! audio or video file comes back as media the model sees ([`media_output`]);
//! any other binary file, a directory, or a file over 32 MB is refused with
//! what to do instead.
//!
//! READ SIZES ITSELF. It used to go through the generic spill, which saved a big
//! read to a file and told the model to Read THAT in pieces: a Read answered with
//! "go Read a copy of what you just read". Now it returns as many whole lines as
//! fit its budget and says where to continue ([`read_text_output`]):
//!   - default budget 25,000 tokens ([`READ_TOKEN_BUDGET`]), Claude Code's own
//!     Read limit, and at most 2,000 lines;
//!   - `allow_large: true` lifts that (and the 2,000-line default) up to the room
//!     the context has left, so "I need the whole file" is one call, not ten;
//!   - neither ever exceeds that room
//!     ([`ToolContext::context_room_tokens`](haest_core::tool::ToolContext::context_room_tokens)),
//!     so a read cannot push the next model call over the request cap.
//!
//! PLUG POINT: [`ReadIntercept`] -- a host answers some paths itself before the
//! workspace fs is asked.

use std::path::Path;

use async_trait::async_trait;
use haest_core::{
    fs::{FsStat, looks_binary},
    media::{media_from_bytes, media_type_from_name},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, req_str},
};
use serde_json::{Value, json};

use crate::canonical::pick;

mod paging;

pub(crate) use paging::clip_line;
pub use paging::{READ_TOKEN_BUDGET, read_text_output};

/// The largest file Read or Edit loads. Either holds the whole file at once,
/// about three times its size while it is decoded and paged, so a bigger one is
/// refused from its stat before any of it is read. Far above any text a model
/// should page through (Read's 25,000-token budget is about 100 KB), and above
/// the 20 MB the largest media may be.
pub(crate) const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Refuse a file over [`MAX_FILE_BYTES`] by its stat, saying what to do instead.
pub(crate) fn within_cap(stat: &FsStat, shown: &str, instead: &str) -> Result<(), ToolError> {
    if stat.size <= MAX_FILE_BYTES {
        return Ok(());
    }
    let mb = |b: u64| b / (1024 * 1024);
    Err(ToolError::Input(format!(
        "{shown} is {} MB, over the {} MB this tool loads; {instead}",
        mb(stat.size),
        mb(MAX_FILE_BYTES)
    )))
}

/// A host's own answer to some Read calls, asked before the workspace fs: media
/// that must go through the host's own caps, an `attachment:<id>` that is no
/// path at all, a document converted to Markdown (page it with
/// [`read_text_output`]).
///
/// # Examples
///
/// ```
/// use async_trait::async_trait;
/// use haest_core::{
///     tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, input},
///     workspace::ShutWorkspace,
/// };
/// use haest_tools::{ReadIntercept, ReadTool, read_text_output};
/// use serde_json::json;
///
/// /// `attachment:<id>` is a document the host keeps, not a file.
/// struct Attachments;
///
/// #[async_trait]
/// impl ReadIntercept for Attachments {
///     async fn intercept(
///         &self,
///         raw_path: &str,
///         input: &JsonMap,
///         ctx: &ToolContext,
///     ) -> Option<Result<ToolOutput, ToolError>> {
///         let id = raw_path.strip_prefix("attachment:")?;
///         let text = format!("# Attachment {id}\nfirst line\n");
///         // Paged like a file: offset, limit and the budget all apply.
///         Some(read_text_output(&text, text.len(), input, ctx, ""))
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let read = ReadTool::new().with_intercept(Attachments);
/// // No files at all: only the intercept can answer.
/// let ctx = ToolContext::new("/", ShutWorkspace);
/// let call = input(json!({"file_path": "attachment:7", "offset": 2}))?;
/// let out = read.execute(call, &ctx).await?;
/// assert_eq!(out.content, "2\tfirst line");
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait ReadIntercept: Send + Sync {
    /// The answer to one Read call, or None to read it from the workspace fs
    /// as usual. `raw_path` is `file_path` as the model sent it, unresolved;
    /// `input` is the whole call (`offset`, `limit`, `allow_large`).
    async fn intercept(
        &self,
        raw_path: &str,
        input: &JsonMap,
        ctx: &ToolContext,
    ) -> Option<Result<ToolOutput, ToolError>>;
}

haest_core::plug_ref! {
    /// A shared [`ReadIntercept`]: what [`ReadTool::with_intercept`] and
    /// [`BuiltinOptions::with_read_intercept`](crate::BuiltinOptions::with_read_intercept)
    /// keep. Either takes the intercept itself.
    pub struct ReadInterceptRef(dyn ReadIntercept);
}

/// A tool result that hands `bytes` to the model as media (an image, PDF,
/// audio or video), and to the transcript as a URL when the host can publish
/// it ([`ToolServices::publish`](haest_core::tool::ToolServices::publish)).
///
/// Read uses it for a media file, WebFetch for a media URL. `label` names it
/// in the text the model reads (`label: image (image/png, 120 bytes),
/// attached.`); its last path segment names the media part.
///
/// # Errors
///
/// [`ToolError::Input`] when `media_type` is not one haest sends to a model,
/// or `bytes` is over that kind's size limit.
pub async fn media_output(
    label: &str,
    bytes: Vec<u8>,
    media_type: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let name = Path::new(label)
        .file_name()
        .map_or_else(|| label.to_string(), |n| n.to_string_lossy().into_owned());
    let part =
        media_from_bytes(&bytes, media_type, &name).map_err(|e| ToolError::Input(e.to_string()))?;
    let len = bytes.len();
    let kind = part.kind.as_str();
    let mut result = json!({"kind": "file", "mediaType": media_type, "bytes": len});
    if let Some(url) = ctx.services().publish(bytes, media_type).await {
        result["url"] = Value::String(url);
    }
    Ok(ToolOutput {
        content: format!("{label}: {kind} ({media_type}, {len} bytes), attached."),
        media: vec![part],
        result: Some(result),
        ..Default::default()
    })
}

/// The Read tool (`Read`, kind `file.read`, read-only): one file from the
/// workspace, optionally with a host's [`ReadIntercept`] in front of the fs.
///
/// Input: `file_path` (absolute, or relative to the context's `cwd`), and
/// optional `offset` (1-based first line), `limit` (lines) and `allow_large`.
/// The module docs say how a read sizes itself.
///
/// A missing file, a directory, a binary file that is not media, a file over
/// 32 MB: each is a [`ToolError::Input`] that tells the model what to do
/// instead. A byte-order mark is dropped.
#[derive(Debug, Default, Clone)]
pub struct ReadTool {
    intercept: Option<ReadInterceptRef>,
}

impl ReadTool {
    /// Read from the workspace fs, with no intercept.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask `intercept` first on every call: the [`ReadIntercept`] example
    /// has one.
    #[must_use]
    pub fn with_intercept(mut self, intercept: impl Into<ReadInterceptRef>) -> Self {
        self.intercept = Some(intercept.into());
        self
    }

    /// Read with `intercept` when there is one: what
    /// [`builtin_tools`](crate::builtin_tools) builds.
    pub(crate) fn intercepted_by(intercept: Option<ReadInterceptRef>) -> Self {
        Self { intercept }
    }

    /// Read from the workspace fs: the tool without its intercept.
    async fn read_fs(&self, input: &JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let fs = ctx.workspace().fs();
        let path = ctx.resolve(req_str(input, "file_path")?);
        let shown = path.display().to_string();
        let Ok(stat) = fs.stat(&path).await else {
            return Err(ToolError::Input(format!("File does not exist: {shown}")));
        };
        if stat.is_dir() {
            return Err(ToolError::Input(format!(
                "{shown} is a directory; use Glob or Bash ls"
            )));
        }
        within_cap(
            &stat,
            &shown,
            "use Grep to find what you need, or Bash (head, tail, sed -n) for a part",
        )?;
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
        read_text_output(text, buf.len(), input, ctx, "")
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
