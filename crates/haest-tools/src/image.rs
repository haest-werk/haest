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
//! PLUG POINT: [`GenerateImageOptions::with_http`] (any
//! [`HttpClient`](haest_http::HttpClient)) and the API key are the host's;
//! haest never reads a key from the environment.
//!
//! Start from [`GenerateImageOptions::new`] and [`GenerateImageTool::new`]. GenerateImage
//! is not one of the [`builtin_tools`](crate::builtin_tools): a host adds it
//! when it has a key.

mod catalog;
mod files;
mod request;
mod settings;
#[cfg(test)]
mod tests;
mod tool;

use std::{fmt, sync::Arc};

use catalog::ImageCatalog;
use haest_core::__private::OPENROUTER_URL;
use haest_http::{
    __private::{REDACTED, UrlDebug},
    HttpClientRef, ReqwestClient,
};
pub use tool::GenerateImageTool;

/// The model GenerateImage uses unless told otherwise, as an OpenRouter
/// model id.
pub const DEFAULT_IMAGE_MODEL: &str = "google/gemini-nano-banana-2.1";

/// The models a GenerateImage call may pick from unless told otherwise: the
/// default, one for exact text and transparency, one for edits from several
/// references.
pub const DEFAULT_IMAGE_MODELS: [&str; 3] = [
    DEFAULT_IMAGE_MODEL,
    "openai/gpt-image-2.6-sunburst",
    "black-forest-labs/flux-3-image",
];

/// What GenerateImage runs on. Clones share the model list read so far.
/// Its Debug output shows neither the key nor a credential in the base URL
/// (its userinfo, a query's values).
///
/// ```
/// use haest_tools::GenerateImageOptions;
///
/// let opts = GenerateImageOptions::new("sk-...")
///     .with_base_url("https://images.example.com/v1")
///     .with_models("vendor/fast", ["vendor/sharp"]);
/// assert_eq!(opts.model(), "vendor/fast");
/// assert_eq!(opts.models(), ["vendor/fast", "vendor/sharp"]);
/// // The key is never printed.
/// assert!(!format!("{opts:?}").contains("sk-"));
/// ```
#[derive(Clone)]
pub struct GenerateImageOptions {
    /// The Image API's key, supplied by the host.
    api_key: String,
    /// Without a trailing slash: `/images` and `/images/models` are appended.
    base_url: String,
    /// The default model.
    model: String,
    /// Every model a call may pick, the default first.
    models: Vec<String>,
    /// The client for the model list and the image request.
    http: HttpClientRef,
    /// The model list read so far, shared by clones of these options.
    catalog: Arc<ImageCatalog>,
}

impl GenerateImageOptions {
    /// The defaults, with the host's key: OpenRouter's Image API,
    /// [`DEFAULT_IMAGE_MODEL`] among [`DEFAULT_IMAGE_MODELS`], the shared
    /// [`ReqwestClient::new`] (built on the first request, never here).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: OPENROUTER_URL.into(),
            model: DEFAULT_IMAGE_MODEL.into(),
            models: DEFAULT_IMAGE_MODELS.map(String::from).to_vec(),
            http: ReqwestClient::new().into(),
            catalog: Arc::default(),
        }
    }

    /// The Image API at `base_url`, without a trailing slash: `/images` and
    /// `/images/models` are appended.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// `model` as the default, then the `others` a call may pick (the default is never listed twice).
    #[must_use]
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

    /// Send the model list and the image requests through `http`: the
    /// client itself, or a handle the host shares.
    #[must_use]
    pub fn with_http(mut self, http: impl Into<HttpClientRef>) -> Self {
        self.http = http.into();
        self
    }

    /// The Image API's base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The default model.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Every model a call may pick, the default first.
    pub fn models(&self) -> &[String] {
        &self.models
    }
}

/// Never the key; the base URL without its userinfo, query values or
/// fragment.
impl fmt::Debug for GenerateImageOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenerateImageOptions")
            .field("api_key", &REDACTED)
            .field("base_url", &UrlDebug(&self.base_url))
            .field("model", &self.model)
            .field("models", &self.models)
            .finish_non_exhaustive()
    }
}
