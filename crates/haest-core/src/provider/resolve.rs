//! Resolving a `provider:model` spec to the provider that serves it.

use std::fmt;

use super::{NO_RESPONSE, ProviderEvent, ProviderRef, ProviderRequest, final_message};
use crate::{
    error::BoxError,
    messages::{AssistantMessage, StopReason},
};

/// A spec resolved to the provider that serves it and the provider's model id.
#[derive(Clone)]
pub struct ResolvedModel {
    provider: ProviderRef,
    model: String,
}

impl ResolvedModel {
    /// `model` on `provider`: the provider itself, or an `Arc` of one the
    /// resolver keeps.
    pub fn new(provider: impl Into<ProviderRef>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// The provider that serves the spec.
    pub fn provider(&self) -> &ProviderRef {
        &self.provider
    }

    /// The id that provider knows the model by (what
    /// [`ProviderRequest::model`](super::ProviderRequest::model) gets).
    pub fn model(&self) -> &str {
        &self.model
    }

    /// This model's whole answer to `req`, for a call outside the loop (a
    /// title, a one-shot question). The request goes to this model: its
    /// `model` is set to [`model`](Self::model).
    ///
    /// It never fails: an API or network error is an answer with
    /// [`StopReason::Error`](crate::StopReason::Error) and the reason in
    /// `error_message`, as a provider sends it; so is a stream that ends
    /// without its answer.
    ///
    /// ```
    /// use haest_core::{
    ///     AssistantMessage, AssistantPart, Message, Provider, ProviderRequest, ProviderStream,
    ///     ResolvedModel, answer_stream,
    /// };
    ///
    /// /// Names everything "Tides".
    /// struct Titler;
    ///
    /// impl Provider for Titler {
    ///     fn id(&self) -> &str {
    ///         "titler"
    ///     }
    ///     fn stream(&self, req: ProviderRequest) -> ProviderStream {
    ///         let mut answer = AssistantMessage::start(self.id(), &req.model);
    ///         answer_stream(|_deltas| async move {
    ///             answer.content.push(AssistantPart::Text { text: "Tides".into() });
    ///             answer
    ///         })
    ///     }
    /// }
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let titles = ResolvedModel::new(Titler, "titles-1");
    /// let req = ProviderRequest::new(titles.model(), vec![Message::user("A title for this?")]);
    /// let answer = titles.complete(req).await;
    /// assert_eq!((answer.text().as_str(), answer.model.as_str()), ("Tides", "titles-1"));
    /// # }
    /// ```
    pub async fn complete(&self, req: ProviderRequest) -> AssistantMessage {
        self.complete_streaming(req, |_| {}).await
    }

    /// The same, handing each event before the answer (text, thinking, a
    /// tool call starting) to `on_delta` as it arrives.
    pub async fn complete_streaming(
        &self,
        mut req: ProviderRequest,
        on_delta: impl FnMut(ProviderEvent),
    ) -> AssistantMessage {
        req.model.clone_from(&self.model);
        let answer = final_message(self.provider.stream(req), on_delta).await;
        answer.unwrap_or_else(|| {
            let mut m = AssistantMessage::start(self.provider.id(), &self.model);
            m.stop_reason = StopReason::Error;
            m.error_message = Some(NO_RESPONSE.into());
            m
        })
    }
}

/// Always this model, whatever the spec: a host with one model resolves
/// every spec to it (`SharedDeps::new(ResolvedModel::new(provider, "m"), ..)`).
impl ModelResolver for ResolvedModel {
    fn resolve(&self, _spec: &str) -> Result<ResolvedModel, ModelError> {
        Ok(self.clone())
    }
}

/// One provider serves every spec: the model is the part after the first
/// colon ([`parse_model_spec`]), and the provider part is not checked
/// (`SharedDeps::new(ProviderRef::new(provider), ..)`).
impl ModelResolver for ProviderRef {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        let (_, model) = parse_model_spec(spec)?;
        Ok(ResolvedModel::new(self.clone(), model))
    }
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
/// [`parse_model_spec`] refuse. The loop shows it to the human with its
/// causes ([`report`](crate::error::report)).
///
/// The variant says what to do: [`BadSpec`](Self::BadSpec) and
/// [`UnknownProvider`](Self::UnknownProvider), pick another model;
/// [`MissingKey`](Self::MissingKey), hand in that key;
/// [`Unavailable`](Self::Unavailable), fix the provider's configuration
/// (its source says what is wrong).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    /// The provider needs a key the host's [`KeySource`](crate::KeySource)
    /// does not have.
    #[error("provider \"{provider}\" needs {key}, and the host has none")]
    MissingKey {
        /// The provider.
        provider: String,
        /// The name the host was asked for (`OPENROUTER_API_KEY`).
        key: String,
    },
    /// The provider is known but cannot be built: its configuration is bad
    /// (a providers.json entry, say). Only specs of that provider fail.
    #[error("provider \"{provider}\" cannot be used")]
    Unavailable {
        /// The provider.
        provider: String,
        /// What is wrong with it: the resolver's own reason.
        #[source]
        source: BoxError,
    },
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
/// use haest_core::{
///     ModelError, ModelResolver, Provider, ProviderRef, ResolvedModel, provider::parse_model_spec,
/// };
/// # use haest_core::{ProviderRequest, ProviderStream};
/// # struct Echo;
/// # impl Provider for Echo {
/// #     fn id(&self) -> &str { "echo" }
/// #     fn stream(&self, _: ProviderRequest) -> ProviderStream { unimplemented!() }
/// # }
///
/// # fn main() -> Result<(), ModelError> {
/// let echo = ProviderRef::new(Echo);
/// let resolver = move |spec: &str| match parse_model_spec(spec)? {
///     ("echo", model) => Ok(ResolvedModel::new(echo.clone(), model)),
///     (other, _) => Err(ModelError::UnknownProvider {
///         provider: other.into(),
///         known: vec!["echo".into()],
///     }),
/// };
///
/// assert_eq!(resolver.resolve("echo:echo-1")?.model(), "echo-1");
/// assert!(matches!(
///     resolver.resolve("acme:big"),
///     Err(ModelError::UnknownProvider { provider, .. }) if provider == "acme"
/// ));
/// # Ok(())
/// # }
/// ```
pub trait ModelResolver: Send + Sync {
    /// The provider and model id for `spec`.
    ///
    /// # Errors
    ///
    /// [`ModelError`]: a bad spec, an unknown provider, a missing key, a
    /// provider that cannot be built.
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

crate::plug_ref! {
    /// A [`ModelResolver`], shared: what [`SharedDeps::models`](crate::SharedDeps::models)
    /// holds. Built from the resolver itself (a closure, `haest-providers`'
    /// registry), or from an `Arc` of one.
    pub struct ModelResolverRef(dyn ModelResolver);
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
