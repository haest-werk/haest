//! GenerateImage's files: the reference images it reads from the workspace,
//! and the picture it writes back, publishes and shows.

use std::path::{Path, PathBuf};

use haest_core::{
    fs::ensure_dir,
    media::{media_from_bytes, media_type_from_name},
    messages::MediaPart,
    now_ms,
    tool::{ToolContext, ToolError, ToolOutput, slug, spelled},
};
use serde_json::{Value, json};

use super::request::ImageReply;

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reference images for an edit: read from the workspace as media parts.
pub(super) async fn references(
    paths: Option<&Value>,
    ctx: &ToolContext,
) -> Result<Vec<MediaPart>, ToolError> {
    let paths = match paths {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => {
            let why = "\"reference_images\" must be an array of file paths";
            return Err(ToolError::Input(why.into()));
        }
    };
    let fs = ctx.workspace().fs();
    let mut out = Vec::new();
    for p in paths {
        let path = ctx.resolve(&spelled(p));
        let shown = path.display();
        if !fs.exists(&path).await {
            return Err(ToolError::Input(format!(
                "Reference image does not exist: {shown}"
            )));
        }
        let Some(media_type) =
            media_type_from_name(&file_name(&path)).filter(|t| t.starts_with("image/"))
        else {
            return Err(ToolError::Input(format!(
                "{shown} is not a png/jpg/gif/webp image"
            )));
        };
        let part = media_from_bytes(&fs.read(&path).await?, media_type, &file_name(&path));
        out.push(part.map_err(|e| ToolError::Input(e.to_string()))?);
    }
    Ok(out)
}

/// The extension the bytes actually have.
fn ext_of(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        // Not in the TS haest's table, which saved a GIF reply under a .png name.
        "image/gif" => "gif",
        _ => "png",
    }
}

/// The path to write: the model's choice with the extension the bytes actually have.
pub(super) fn output_path(
    ctx: &ToolContext,
    requested: Option<&str>,
    prompt: &str,
    media_type: &str,
) -> PathBuf {
    let ext = ext_of(media_type);
    let base = match requested {
        Some(p) => ctx.resolve(p),
        None => {
            let ms = now_ms();
            ctx.resolve(&format!(
                "generated/{}-{ms}.{ext}",
                slug(prompt, 40, "image")
            ))
        }
    };
    let has = base
        .extension()
        .is_some_and(|e| e.to_string_lossy().to_ascii_lowercase() == ext);
    if has { base } else { base.with_extension(ext) }
}

/// Write it, publish it to the transcript, and give it back to the model to look at.
pub(super) async fn save_and_show(
    ctx: &ToolContext,
    reply: ImageReply,
    path: &Path,
    takes: &str,
) -> Result<ToolOutput, ToolError> {
    let fs = ctx.workspace().fs();
    if let Some(dir) = path.parent() {
        ensure_dir(fs.as_ref(), dir).await?;
    }
    fs.write(path, &reply.bytes).await?;
    let (len, mt, model) = (
        reply.bytes.len(),
        reply.media_type.as_str(),
        reply.model.as_str(),
    );
    let part = media_from_bytes(&reply.bytes, mt, &file_name(path)).ok();
    let url = ctx.services().publish(reply.bytes, mt).await;
    let cost = reply
        .cost_usd
        .map_or_else(String::new, |c| format!(", ${c:.3}"));
    let mut result = json!({"kind": "file", "mediaType": mt, "bytes": len});
    if let Some(url) = url {
        result["url"] = url.into();
    }
    Ok(ToolOutput {
        content: format!(
            "Saved {} ({model}, {mt}, {len} bytes{cost}). {model} takes: {takes}.",
            path.display()
        ),
        media: part.into_iter().collect(),
        cost_usd: reply.cost_usd,
        result: Some(result),
        ..Default::default()
    })
}
