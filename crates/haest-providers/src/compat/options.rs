//! [`CompatOptions`]: how a [`CompatProvider`](super::CompatProvider) reaches
//! its server and what it sends.

use std::{collections::HashMap, fmt};

use haest_core::catalog::{ModelPricing, Reasoning};

use super::CompatFlavor;
use crate::{
    routing::RoutingScopes,
    send::{HeaderNames, REDACTED, UrlDebug},
};

/// How a [`CompatProvider`](super::CompatProvider) reaches its server and what it sends.
///
/// [`CompatOptions::new`] takes the two every provider needs and leaves the
/// rest plain ([`CompatFlavor::Generic`], no extras); set the others with the
/// `with_*` setters. Its Debug output shows neither the key, nor a header's
/// value, nor a `user:password@` in the base URL.
///
/// ```
/// use haest_providers::{CompatFlavor, CompatOptions};
///
/// let opts = CompatOptions::new("openrouter", "sk-or-...")
///     .with_flavor(CompatFlavor::OpenRouter)
///     .with_base_url("https://gateway.example.com/v1");
/// assert!(!format!("{opts:?}").contains("sk-or"));
/// ```
#[derive(Clone)]
pub struct CompatOptions {
    pub(crate) id: String,
    pub(crate) flavor: CompatFlavor,
    /// Sent as `Authorization: Bearer <api_key>`.
    pub(crate) api_key: String,
    pub(crate) base_url: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) reasoning_effort: Option<Reasoning>,
    pub(crate) pricing: HashMap<String, ModelPricing>,
    pub(crate) routing: Option<RoutingScopes>,
}

impl CompatOptions {
    /// The provider `id`, sending `api_key` to OpenAI's own URL with the plain
    /// body ([`CompatFlavor::Generic`]) and nothing else configured.
    pub fn new(id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            flavor: CompatFlavor::Generic,
            api_key: api_key.into(),
            base_url: None,
            headers: Vec::new(),
            reasoning_effort: None,
            pricing: HashMap::new(),
            routing: None,
        }
    }

    /// The provider's name: its [`Provider::id`](haest_core::Provider::id),
    /// stored on every message it answers. Reasoning replays only to the
    /// provider whose id a message carries.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Where requests go: `<base_url>/chat/completions`; None =
    /// `https://api.openai.com/v1` ([`with_base_url`](Self::with_base_url)).
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// These options with OpenRouter's extras, or the plain body.
    #[must_use]
    pub fn with_flavor(mut self, flavor: CompatFlavor) -> Self {
        self.flavor = flavor;
        self
    }

    /// These options sending to `url` (`<url>/chat/completions`, the path
    /// before `url`'s query) instead of OpenAI's own. Its path shows in
    /// Debug output and errors: a gateway's key goes in a header
    /// ([`with_header`](Self::with_header)) or the query, never the path.
    ///
    /// A URL with an `@` past its host, or with no host (`https:///v1`,
    /// what an empty `${HOST}` makes), is never sent to: every request ends
    /// as an error saying so (haest-http's `check_endpoint_url`). A
    /// password holding an unescaped `/`, `?` or `#` ends the host a URL
    /// parser reads there, so it goes in percent-encoded (`/` as `%2F`).
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// These options sending `name: value` on every request, after the ones
    /// before it.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// These options sending a top-level `reasoning_effort`, fitted to the
    /// nearest of the levels `reasoning` lists (off only when it lists
    /// `none`). Without it no `reasoning_effort` is sent.
    #[must_use]
    pub fn with_reasoning_effort(mut self, reasoning: Reasoning) -> Self {
        self.reasoning_effort = Some(reasoning);
        self
    }

    /// These options with `pricing` added, by model id, over the prices
    /// already set: the cost of an answer from a server that reports none.
    #[must_use]
    pub fn with_pricing(
        mut self,
        pricing: impl IntoIterator<Item = (impl Into<String>, ModelPricing)>,
    ) -> Self {
        let rows = pricing.into_iter().map(|(model, row)| (model.into(), row));
        self.pricing.extend(rows);
        self
    }

    /// These options sending OpenRouter provider routing (OpenRouter only),
    /// resolved per request against the request's model. Without it no
    /// `provider` object is sent (a host's gateway may route itself).
    #[must_use]
    pub fn with_routing(mut self, routing: RoutingScopes) -> Self {
        self.routing = Some(routing);
        self
    }
}

impl fmt::Debug for CompatOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompatOptions")
            .field("id", &self.id)
            .field("flavor", &self.flavor)
            .field("api_key", &REDACTED)
            .field("base_url", &self.base_url.as_deref().map(UrlDebug))
            .field("headers", &HeaderNames(&self.headers))
            .field("reasoning_effort", &self.reasoning_effort)
            .field("pricing", &self.pricing)
            .field("routing", &self.routing)
            .finish()
    }
}
