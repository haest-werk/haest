//! Resolving a `provider:model` spec to the provider that serves it.

use std::{fmt, sync::Arc};

use super::Provider;

/// A spec resolved to the provider that serves it and the provider's model id.
#[derive(Clone)]
pub struct ResolvedModel {
    /// The provider that serves the spec.
    pub provider: Arc<dyn Provider>,
    /// The id that provider knows the model by (what [`ProviderRequest::model`](super::ProviderRequest::model) gets).
    pub model: String,
}

impl fmt::Debug for ResolvedModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedModel")
            .field("provider", &self.provider.id())
            .field("model", &self.model)
            .finish()
    }
}

/// Why a model spec cannot be served: what [`ModelResolver::resolve`] and
/// [`parse_model_spec`] refuse. The loop shows its text to the human.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The spec is not `<provider>:<model>`.
    #[error(
        "model \"{spec}\" must be \"<provider>:<model>\", e.g. openrouter:anthropic/claude-sonnet-5"
    )]
    BadSpec {
        /// The spec, as written.
        spec: String,
    },
    /// No provider goes by the spec's provider part.
    #[error("unknown provider \"{provider}\". Known: {}", known.join(", "))]
    UnknownProvider {
        /// The provider part, as written.
        provider: String,
        /// The providers there are.
        known: Vec<String>,
    },
    /// The provider is known but cannot be built: a key it needs is not set,
    /// its configuration is bad. Its text is the resolver's own.
    #[error(transparent)]
    Unavailable(Box<dyn std::error::Error + Send + Sync>),
}

/// Turns a `provider:model` spec into a live provider. The loop asks it before
/// every model call, so a model switch mid-conversation takes effect at once.
///
/// Any `Fn(&str) -> Result<ResolvedModel, ModelError>` is one; `haest-providers`'
/// registry is the full one.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::{ModelError, ModelResolver, Provider, ResolvedModel, provider::parse_model_spec};
/// # use haest_core::{ProviderRequest, provider::ProviderStream};
/// # struct Echo;
/// # impl Provider for Echo {
/// #     fn id(&self) -> &str { "echo" }
/// #     fn stream(&self, _: ProviderRequest) -> ProviderStream { unimplemented!() }
/// # }
///
/// let echo: Arc<dyn Provider> = Arc::new(Echo);
/// let resolver = move |spec: &str| match parse_model_spec(spec)? {
///     ("echo", model) => Ok(ResolvedModel { provider: echo.clone(), model: model.into() }),
///     (other, _) => Err(ModelError::UnknownProvider {
///         provider: other.into(),
///         known: vec!["echo".into()],
///     }),
/// };
///
/// assert_eq!(resolver.resolve("echo:echo-1").unwrap().model, "echo-1");
/// assert!(matches!(
///     resolver.resolve("acme:big"),
///     Err(ModelError::UnknownProvider { provider, .. }) if provider == "acme"
/// ));
/// ```
pub trait ModelResolver: Send + Sync {
    /// The provider and model id for `spec`.
    ///
    /// # Errors
    ///
    /// [`ModelError`]: a bad spec, an unknown provider, one that cannot be
    /// built (a missing key).
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError>;
}

impl<F> ModelResolver for F
where
    F: Fn(&str) -> Result<ResolvedModel, ModelError> + Send + Sync,
{
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        self(spec)
    }
}

/// Splits a model spec into provider and model id at the FIRST colon:
/// `openrouter:anthropic/claude-sonnet-5` -> ("openrouter", "anthropic/claude-sonnet-5"),
/// `openrouter:x/y:free` -> ("openrouter", "x/y:free").
///
/// # Errors
///
/// [`ModelError::BadSpec`] when there is no colon or nothing before it.
pub fn parse_model_spec(spec: &str) -> Result<(&str, &str), ModelError> {
    match spec.find(':') {
        Some(i) if i > 0 => Ok((&spec[..i], &spec[i + 1..])),
        _ => Err(ModelError::BadSpec { spec: spec.into() }),
    }
}
