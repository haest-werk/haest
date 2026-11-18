//! The GenerateImage tool: check the call, pay for one picture, save it, show it.

use async_trait::async_trait;
use haest_core::{
    error::report,
    media::data_url,
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str},
};
use haest_http::check_endpoint_url;
use serde_json::{Map, Value, json};

use super::{
    GenerateImageOptions,
    files::{output_path, references, save_and_show},
    request::call_image_api,
    settings::{check_image_settings, describe_model, max_references},
};
use crate::canonical::pick;

/// The GenerateImage tool (`GenerateImage`, kind `image.generate`): one
/// picture from a prompt, or an edit of reference images, with an image model.
///
/// Input: `prompt`, and optional `file_path` (default
/// `generated/<slug>-<ms>.<ext>` under the context's `cwd`),
/// `reference_images` (paths in the workspace), `model` (one of
/// [`GenerateImageOptions::models`]) and `settings` (the model's own: aspect
/// ratio, resolution, ...).
///
/// Before anything is paid for, the model and every setting are checked
/// against the Image API's model list (read on the first call, kept an hour,
/// shared by clones of the options); a refusal tells the model what the
/// chosen image model takes. Then one request
/// is sent, and the picture is written into the workspace under the extension
/// its bytes have (over a file that is there, whole or not at all, as
/// [`WriteTool`](crate::WriteTool) writes), published to the host, and
/// attached so the model sees it.
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
/// use std::path::Path;
///
/// use async_trait::async_trait;
/// use haest_core::{
///     fs::{Fs, FsRef, MemFs},
///     tool::{Tool, ToolContext, input},
/// };
/// use haest_tools::{
///     GenerateImageTool, GenerateImageOptions,
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
///             return Ok(Response::new(200, "").with_json(&models));
///         }
///         assert_eq!(req.json_body()["aspect_ratio"], "16:9");
///         // A PNG signature is enough to count as a PNG.
///         let image = json!({"data": [{"b64_json": "iVBORw0KGgo="}], "usage": {"cost": 0.04}});
///         Ok(Response::new(200, "").with_json(&image))
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let opts = GenerateImageOptions::new("the host's key")
///     .with_http(FakeApi)
///     .with_models("vendor/pic", ["vendor/other"]);
/// let tool = GenerateImageTool::new(opts);
/// // The host keeps a handle on the files, to look at them.
/// let files = FsRef::new(MemFs::new());
/// let ctx = ToolContext::new("/work", SandboxWorkspace::over(files.clone()));
///
/// let call = json!({
///     "prompt": "a lighthouse at dusk",
///     "file_path": "art/lighthouse",
///     "settings": {"aspect_ratio": "16:9"}
/// });
/// let out = tool.execute(input(call)?, &ctx).await?;
/// assert_eq!(out.cost_usd, Some(0.04));
/// assert!(files.exists(Path::new("/work/art/lighthouse.png")).await);
///
/// // A setting the model does not take is refused before anything is sent.
/// let call = json!({"prompt": "x", "settings": {"quality": "high"}});
/// assert!(tool.execute(input(call)?, &ctx).await.is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct GenerateImageTool {
    opts: GenerateImageOptions,
}

impl GenerateImageTool {
    /// GenerateImage over `opts`.
    pub fn new(opts: GenerateImageOptions) -> Self {
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

#[async_trait]
impl Tool for GenerateImageTool {
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
        check_endpoint_url(&o.base_url).map_err(|e| ToolError::Failed(e.to_string()))?;
        let prompt = req_str(&input, "prompt")?;
        let chosen = self.pick_model(&input)?;
        let given = settings_input(&input)?;
        let refs = references(input.get("reference_images"), ctx).await?;
        let (http, cancel) = (&*o.http, ctx.cancel());
        let params = o
            .catalog
            .model_params(http, &o.base_url, &chosen, cancel)
            .await
            .map_err(|e| ToolError::Input(report(&e)))?;
        let params = params.as_ref();
        let takes = describe_model(params);
        let settings = check_image_settings(&chosen, params, &given)
            .map_err(|e| ToolError::Input(report(&e)))?;
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
        let body = Value::Object(body);
        let reply = call_image_api(http, &o.base_url, &o.api_key, &chosen, &body, cancel)
            .await
            .map_err(|e| ToolError::Failed(report(&e)))?;
        let path = output_path(
            ctx,
            opt_str(&input, "file_path")?,
            prompt,
            &reply.media_type,
        );
        save_and_show(ctx, reply, &path, &takes).await
    }
}
