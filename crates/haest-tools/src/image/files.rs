//! GenerateImage's files: the reference images it reads from the workspace,
//! and the picture it writes back, publishes and shows.

use std::path::{Path, PathBuf};

use haest_core::{
    fs::Fs,
    media::{media_from_bytes, media_type_from_name},
    messages::MediaPart,
    now_ms,
    tool::{ToolContext, ToolError, ToolOutput, slug, spelled},
};
use serde_json::{Value, json};

use super::request::ImageReply;
use crate::files::save;

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

/// Where the picture goes.
pub(super) struct Output {
    path: PathBuf,
    /// The model named this very path, extension and all, so the picture
    /// may go over a file there. A path the tool chose (the default, or
    /// the model's with the extension switched to the bytes') never does.
    named: bool,
}

/// The path to write: the model's choice with the extension the bytes actually have.
pub(super) fn output_path(
    ctx: &ToolContext,
    requested: Option<&str>,
    prompt: &str,
    media_type: &str,
) -> Output {
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
    let named = requested.is_some() && has;
    let path = if has { base } else { base.with_extension(ext) };
    Output { path, named }
}

/// How many names beside a taken one are tried: `<stem>-2.<ext>` on.
const FREE_NAMES: u32 = 1000;

/// `out`'s path, or where it is taken and the model did not name it, the
/// first free `<stem>-<n>.<ext>` beside it. A dangling link takes its name:
/// a write would make the file it names.
async fn free_path(fs: &dyn Fs, out: Output) -> Result<PathBuf, ToolError> {
    let taken = async |p: &Path| fs.exists(p).await || fs.read_link(p).await.is_ok();
    if out.named || !taken(&out.path).await {
        return Ok(out.path);
    }
    let (stem, ext) = (out.path.file_stem(), out.path.extension());
    let (stem, ext) = (stem.unwrap_or_default(), ext.unwrap_or_default());
    for n in 2..FREE_NAMES {
        let mut name = stem.to_os_string();
        name.push(format!("-{n}."));
        name.push(ext);
        let path = out.path.with_file_name(name);
        if !taken(&path).await {
            return Ok(path);
        }
    }
    let shown = out.path.display();
    Err(ToolError::Failed(format!(
        "{shown} and the names beside it are taken; name a file_path for the picture"
    )))
}

/// Write it, publish it to the transcript, and give it back to the model to look at.
pub(super) async fn save_and_show(
    ctx: &ToolContext,
    reply: ImageReply,
    out: Output,
    takes: &str,
) -> Result<ToolOutput, ToolError> {
    let fs = ctx.workspace().fs();
    let wanted = file_name(&out.path);
    let path = &free_path(fs.as_ref(), out).await?;
    let beside = if file_name(path) == wanted {
        String::new()
    } else {
        format!(" {wanted} is taken, so it went beside it.")
    };
    // Over an image that is there, whole or not at all, as Write does.
    save(fs.as_ref(), path, &reply.bytes).await?;
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
            "Saved {} ({model}, {mt}, {len} bytes{cost}).{beside} {model} takes: {takes}.",
            path.display()
        ),
        media: part.into_iter().collect(),
        cost_usd: reply.cost_usd,
        result: Some(result),
        ..Default::default()
    })
}
