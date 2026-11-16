//! GenerateImage: image OUTPUT for any haest model, through an Image API
//! (`POST /images`; OpenRouter's by default). The conversation's model stays
//! whatever the human picked and calls a dedicated image model here; which ones
//! it may pick is the host's list ([`GenerateImageOptions::models`]). Every setting
//! is checked against the chosen model's own list before anything is paid for
//! ([`settings`]), and every refusal says what the model takes.
//!
//! The image is written into the workspace (so the next step can Read, move or
//! commit it), published to the host so the transcript shows it, and handed
//! back as media so the model can SEE what it made and iterate.
//!
//! Spend: returned as the output's `cost_usd`, which the loop adds to the turn
//! and charges to the ledger, so the turn total and the caps both count it.
//! Ported from the TS haest's `tools/image.ts`.
//!
//! PLUG POINT: [`GenerateImageOptions::http`] (any [`HttpClient`]) and the API key
//! are the host's; haest never reads a key from the environment.
//!
//! Start from [`GenerateImageOptions::new`] and [`GenerateImageTool::new`]. GenerateImage
//! is not one of the [`builtin_tools`](crate::builtin_tools): a host adds it
//! when it has a key.

mod catalog;
mod files;
mod request;
pub mod settings;
#[cfg(test)]
mod tests;
mod tool;

use std::{fmt, sync::Arc};

pub use catalog::ImageCatalog;
pub use haest_core::__private::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS};
use haest_core::handed::OPENROUTER_URL;
use haest_http::{HttpClient, ReqwestClient};
pub use request::{ImageError, ImageReply, call_image_api, sniff_image_type};
pub use tool::GenerateImageTool;

/// What GenerateImage runs on. Clones share the model list read so far.
#[derive(Clone)]
pub struct GenerateImageOptions {
    /// The Image API's key, supplied by the host.
    pub api_key: String,
    /// Without a trailing slash: `/images` and `/images/models` are appended.
    pub base_url: String,
    /// The default model.
    pub model: String,
    /// Every model a call may pick, the default first.
    pub models: Vec<String>,
    /// The client for the model list and the image request.
    pub http: Arc<dyn HttpClient>,
    /// The model list read so far, shared by clones of these options.
    pub catalog: Arc<ImageCatalog>,
}

impl GenerateImageOptions {
    /// The defaults, with the host's key: OpenRouter's Image API,
    /// [`DEFAULT_IMAGE_MODEL`] among [`DEFAULT_IMAGE_MODELS`], the shared
    /// [`ReqwestClient::new`] (built on the first request, never here).
    ///
    /// ```
    /// use haest_tools::GenerateImageOptions;
    ///
    /// let opts = GenerateImageOptions::new("sk-...").with_models("vendor/fast", ["vendor/sharp"]);
    /// assert_eq!(opts.models, ["vendor/fast", "vendor/sharp"]);
    /// // The key is never printed.
    /// assert!(!format!("{opts:?}").contains("sk-"));
    /// ```
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: OPENROUTER_URL.into(),
            model: DEFAULT_IMAGE_MODEL.into(),
            models: DEFAULT_IMAGE_MODELS.map(String::from).to_vec(),
            http: Arc::new(ReqwestClient::new()),
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

impl fmt::Debug for GenerateImageOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenerateImageOptions")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("models", &self.models)
            .finish()
    }
}
