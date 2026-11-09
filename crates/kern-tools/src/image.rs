//! GenerateImage: image OUTPUT for any kern model, through an Image API
//! (`POST /images`; OpenRouter's by default). The conversation's model stays
//! whatever the human picked and calls a dedicated image model here; which ones
//! it may pick is the host's list ([`ImageToolOptions::models`]). Every setting
//! is checked against the chosen model's own list before anything is paid for
//! ([`settings`]), and every refusal says what the model takes.
//!
//! The image is written into the workspace (so the next step can Read, move or
//! commit it), published to the host so the transcript shows it, and handed
//! back as media so the model can SEE what it made and iterate.
//!
//! Spend: returned as the output's `cost_usd`, which the loop adds to the turn
//! and charges to the ledger, so the turn total and the caps both count it.
//! Ported from the TS kern's `tools/image.ts`.
//!
//! PLUG POINT: [`ImageHttp`] (the transport) and the API key are the host's;
//! kern never reads a key from the environment.

mod catalog;
mod request;
pub mod settings;
#[cfg(test)]
mod tests;
mod tool;

use std::{fmt, sync::Arc};

pub use catalog::ImageCatalog;
pub use request::{
    HttpReply, ImageHttp, ImageReply, ReqwestImageHttp, call_image_api, sniff_image_type,
};
pub use tool::ImageTool;

pub const DEFAULT_IMAGE_MODEL: &str = "google/gemini-nano-banana-2.1";
/// What a call may pick by default: the default, exact text + transparency, multi-reference edits.
pub const DEFAULT_IMAGE_MODELS: [&str; 3] = [
    DEFAULT_IMAGE_MODEL,
    "openai/gpt-image-2.6-sunburst",
    "black-forest-labs/flux-3-image",
];
pub const DEFAULT_IMAGE_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// What GenerateImage runs on. Clones share the model list read so far.
#[derive(Clone)]
pub struct ImageToolOptions {
    /// The Image API's key, supplied by the host.
    pub api_key: String,
    /// Without a trailing slash: `/images` and `/images/models` are appended.
    pub base_url: String,
    /// The default model.
    pub model: String,
    /// Every model a call may pick, the default first.
    pub models: Vec<String>,
    pub http: Arc<dyn ImageHttp>,
    pub catalog: Arc<ImageCatalog>,
}

impl ImageToolOptions {
    /// The defaults, with the host's key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_IMAGE_BASE_URL.into(),
            model: DEFAULT_IMAGE_MODEL.into(),
            models: DEFAULT_IMAGE_MODELS.map(String::from).to_vec(),
            http: Arc::new(ReqwestImageHttp),
            catalog: Arc::default(),
        }
    }

    /// `model` as the default, then the `others` a call may pick (the default is never listed twice).
    pub fn with_models<S: Into<String>>(
        mut self,
        model: impl Into<String>,
        others: impl IntoIterator<Item = S>,
    ) -> Self {
        let model = model.into();
        let others = others.into_iter().map(Into::into).filter(|m| *m != model);
        self.models = std::iter::once(model.clone()).chain(others).collect();
        self.model = model;
        self
    }
}

impl fmt::Debug for ImageToolOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageToolOptions")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("models", &self.models)
            .finish()
    }
}
