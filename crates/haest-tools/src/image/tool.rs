//! The GenerateImage tool: check the call, pay for one picture, save it, show it.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use haest_core::{
    __private::data_url,
    __private::slug,
    fs::ensure_dir,
    media::{media_from_bytes, media_type_from_name},
    messages::MediaPart,
    now_ms,
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str, spelled},
};
use serde_json::{Map, Value, json};

use super::{
    ImageToolOptions,
    request::{ImageReply, call_image_api},
    settings::{check_image_settings, describe_model, max_references},
};
use crate::canonical::pick;

/// The GenerateImage tool (`GenerateImage`, kind `image.generate`): one
/// picture from a prompt, or an edit of reference images, with an image model.
///
/// Input: `prompt`, and optional `file_path` (default
/// `generated/<slug>-<ms>.<ext>` under the context's `cwd`),
/// `reference_images` (paths in the workspace), `model` (one of
/// [`ImageToolOptions::models`]) and `settings` (the model's own: aspect
/// ratio, resolution, ...).
///
/// Before anything is paid for, the model and every setting are checked
/// against the Image API's model list ([`settings`](super::settings)); a
/// refusal tells the model what the chosen image model takes. Then one request
/// is sent, and the picture is written into the workspace under the extension
/// its bytes have, published to the host, and attached so the model sees it.
/// The spend comes back as the output's
/// [`cost_usd`](haest_core::tool::ToolOutput::cost_usd).
///
/// Every refusal is a [`ToolError::Input`]; the Image API's own refusal is a
/// [`ToolError::Failed`] with its words.
///
/// # Examples
///
/// Over a fake Image API, so nothing is paid for:
///
/// ```
/// use std::{path::Path, sync::Arc};
///
/// use async_trait::async_trait;
/// use haest_core::{
///     fs::{Fs, MemFs},
///     tool::{Tool, ToolContext},
/// };
/// use haest_tools::{
///     ImageTool, ImageToolOptions,
///     http::{HttpClient, HttpError, Method, Request, Response},
/// };
/// use haest_workspace::SandboxWorkspace;
/// use serde_json::json;
///
/// /// One model, which takes an aspect ratio and draws a PNG for $0.04.
/// struct FakeApi;
///
/// #[async_trait]
/// impl HttpClient for FakeApi {
///     async fn send(&self, req: Request) -> Result<Response, HttpError> {
///         if req.method == Method::Get {
///             let aspect = json!({"type": "enum", "values": ["1:1", "16:9"]});
///             let models = json!({"data": [
///                 {"id": "vendor/pic", "supported_parameters": {"aspect_ratio": aspect}}
///             ]});
///             return Ok(Response::json(200, &models));
///         }
///         assert_eq!(req.json()["aspect_ratio"], "16:9");
///         // A PNG signature is enough to count as a PNG.
///         let image = json!({"data": [{"b64_json": "iVBORw0KGgo="}], "usage": {"cost": 0.04}});
///         Ok(Response::json(200, &image))
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let opts = ImageToolOptions {
///     http: Arc::new(FakeApi),
///     ..ImageToolOptions::new("the host's key")
/// };
/// let tool = ImageTool::new(opts.with_models("vendor/pic", ["vendor/other"]));
/// let files = Arc::new(MemFs::new());
/// let ctx = ToolContext::new("/work", Arc::new(SandboxWorkspace::over(files.clone())));
///
/// let input = json!({
///     "prompt": "a lighthouse at dusk",
///     "file_path": "art/lighthouse",
///     "settings": {"aspect_ratio": "16:9"}
/// });
/// let out = tool.execute(input.as_object().unwrap().clone(), &ctx).await.unwrap();
/// assert_eq!(out.cost_usd, Some(0.04));
/// assert!(files.exists(Path::new("/work/art/lighthouse.png")).await);
///
/// // A setting the model does not take is refused before anything is sent.
/// let input = json!({"prompt": "x", "settings": {"quality": "high"}});
/// assert!(tool.execute(input.as_object().unwrap().clone(), &ctx).await.is_err());
/// # }
/// ```
pub struct ImageTool {
    opts: ImageToolOptions,
}

impl ImageTool {
    /// GenerateImage over `opts`.
    pub fn new(opts: ImageToolOptions) -> Self {
        Self { opts }
    }

    fn pick_model(&self, input: &JsonMap) -> Result<String, ToolError> {
        let model = opt_str(input, "model")?.unwrap_or(&self.opts.model);
        if !self.opts.models.iter().any(|m| m == model) {
            return Err(ToolError::Input(format!(
                "\"model\" is one of: {} (not {})",
                self.opts.models.join(", "),
                Value::from(model)
            )));
        }
        Ok(model.to_string())
    }
}

fn settings_input(input: &JsonMap) -> Result<Map<String, Value>, ToolError> {
    match input.get("settings") {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m.clone()),
        Some(_) => Err(ToolError::Input(
            "\"settings\" is an object of the model's settings, e.g. {\"aspect_ratio\": \"16:9\"}"
                .into(),
        )),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reference images for an edit: read from the workspace as media parts.
async fn references(paths: Option<&Value>, ctx: &ToolContext) -> Result<Vec<MediaPart>, ToolError> {
    let paths = match paths {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => {
            let why = "\"reference_images\" must be an array of file paths";
            return Err(ToolError::Input(why.into()));
        }
    };
    let fs = ctx.workspace.fs();
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
fn output_path(
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
async fn save_and_show(
    ctx: &ToolContext,
    reply: ImageReply,
    path: &Path,
    takes: &str,
) -> Result<ToolOutput, ToolError> {
    let fs = ctx.workspace.fs();
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
    let url = ctx.services.publish(reply.bytes, mt).await;
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

#[async_trait]
impl Tool for ImageTool {
    fn name(&self) -> &str {
        "GenerateImage"
    }
    fn kind(&self) -> &str {
        "image.generate"
    }
    fn description(&self) -> String {
        let o = &self.opts;
        let others: Vec<&str> = o
            .models
            .iter()
            .filter(|m| **m != o.model)
            .map(String::as_str)
            .collect();
        let choice = if others.is_empty() {
            String::new()
        } else {
            format!(
                " Other models: {} (gpt-image is best at exact text and transparent backgrounds).",
                others.join(", ")
            )
        };
        format!(
            "Generate an image from a text prompt (or edit existing images: pass them as reference_images) with {} by default.{choice} The image is saved to file_path (default generated/<slug>.png), shown to the human, and attached to the result so you can see it. Each call costs money (roughly $0.03-0.20 by model and settings); generate only what was asked for.",
            o.model
        )
    }
    fn parameters(&self) -> Value {
        let mut p = json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "What to draw, or how to change the reference images: subject, layout, style, every word of text in it"},
                "file_path": {"type": "string", "description": "Where to save it (absolute or relative to the working directory)"},
                "reference_images": {"type": "array", "items": {"type": "string"}, "description": "Paths of images to edit or use as reference"},
                "settings": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "The model's own settings, only the ones it takes: aspect_ratio, resolution, size, quality, background, output_format, output_compression, seed. e.g. {\"aspect_ratio\": \"16:9\", \"resolution\": \"2K\"}. Leave it out for square at 1K. A setting the model does not take is refused with the list of what it takes."
                }
            },
            "required": ["prompt"]
        });
        if self.opts.models.len() > 1 {
            p["properties"]["model"] = json!({
                "type": "string",
                "enum": self.opts.models,
                "description": format!("Default {}", self.opts.model)
            });
        }
        p
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        let keys = [
            ("prompt", "prompt"),
            ("path", "file_path"),
            ("references", "reference_images"),
            ("model", "model"),
            ("settings", "settings"),
        ];
        pick(input, &keys)
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let o = &self.opts;
        let prompt = req_str(&input, "prompt")?;
        let chosen = self.pick_model(&input)?;
        let given = settings_input(&input)?;
        let refs = references(input.get("reference_images"), ctx).await?;
        let (http, cancel) = (o.http.as_ref(), ctx.cancel.clone());
        let params = o
            .catalog
            .model_params(&o.base_url, &chosen, http, cancel.clone())
            .await
            .map_err(|e| ToolError::Input(e.to_string()))?;
        let params = params.as_ref();
        let takes = describe_model(params);
        let settings = check_image_settings(&chosen, params, &given)
            .map_err(|e| ToolError::Input(e.message))?;
        let max = max_references(params);
        if params.is_some() && refs.len() as u64 > max {
            let most = if max > 0 {
                format!("at most {max}")
            } else {
                "no".into()
            };
            return Err(ToolError::Input(format!(
                "{chosen} takes {most} reference images (got {}). {chosen} takes: {takes}.",
                refs.len()
            )));
        }
        let mut body = Map::new();
        body.insert("model".into(), chosen.clone().into());
        body.insert("prompt".into(), prompt.into());
        body.extend(settings);
        if !refs.is_empty() {
            let refs = refs
                .iter()
                .map(|r| json!({"type": "image_url", "image_url": {"url": data_url(r)}}));
            body.insert("input_references".into(), refs.collect());
        }
        let reply = call_image_api(
            http,
            &o.base_url,
            &o.api_key,
            &chosen,
            &Value::Object(body),
            cancel,
        )
        .await
        .map_err(|e| ToolError::Failed(crate::report(&e)))?;
        let path = output_path(
            ctx,
            opt_str(&input, "file_path")?,
            prompt,
            &reply.media_type,
        );
        save_and_show(ctx, reply, &path, &takes).await
    }
}
