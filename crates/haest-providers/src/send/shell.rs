//! What the providers over a [`Sender`](super::Sender) have in common,
//! written once: the options of the dialects that take only a key, a base URL,
//! headers and prices ([`endpoint_options`]), and every provider's
//! constructor, setters, id and [`Provider`](haest_core::Provider) impl
//! ([`sender_provider`]).

/// The options type `$name` of a dialect that takes an id, a key, a base URL,
/// headers and prices: the struct, `new`, `id`, `base_url`, `with_base_url`,
/// `with_header`, `with_pricing`, and a Debug that shows neither the key, nor
/// a header's value, nor a `user:password@` in the base URL.
///
/// `api` names the dialect's own API in prose, `url` is its base URL, `path`
/// the path a request goes to under the base URL, and `key` the header the
/// key is sent in.
macro_rules! endpoint_options {
    (
        $(#[$meta:meta])*
        $name:ident {
            api: $api:literal,
            url: $url:literal,
            path: $path:literal,
            key: $key:literal $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone)]
        pub struct $name {
            pub(crate) id: String,
            pub(crate) api_key: String,
            pub(crate) base_url: Option<String>,
            pub(crate) headers: Vec<(String, String)>,
            pub(crate) pricing: ::std::collections::HashMap<String, ::haest_core::catalog::ModelPricing>,
        }

        impl $name {
            #[doc = concat!("The provider `id`, sending `api_key` to ", $api, " with no extra")]
            /// headers.
            pub fn new(id: impl Into<String>, api_key: impl Into<String>) -> Self {
                Self {
                    id: id.into(),
                    api_key: api_key.into(),
                    base_url: None,
                    headers: Vec::new(),
                    pricing: ::std::collections::HashMap::new(),
                }
            }

            /// The provider's name: its [`Provider::id`](haest_core::Provider::id),
            /// stored on every message it answers. Thinking replays only to the
            /// provider whose id a message carries.
            pub fn id(&self) -> &str {
                &self.id
            }

            #[doc = concat!("Where requests go: `<base_url>/", $path, "`; None =")]
            #[doc = concat!("`", $url, "` ([`with_base_url`](Self::with_base_url)).")]
            pub fn base_url(&self) -> Option<&str> {
                self.base_url.as_deref()
            }

            #[doc = concat!("These options sending to `url` (`<url>/", $path, "`, the path before")]
            #[doc = concat!("`url`'s query) instead of ", $api, ". Its path shows in Debug")]
            /// output and errors: a gateway's key goes in a header
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
            /// before it (a proxy that authenticates by header). One named like a
            #[doc = concat!("default (`", $key, "`, in any case) replaces it.")]
            #[must_use]
            pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
                self.headers.push((name.into(), value.into()));
                self
            }

            /// These options with `pricing` added, by model id, over the prices
            #[doc = concat!("already set: the cost of an answer, which ", $api, " does not")]
            /// report. A model with no price is booked by its tokens only.
            #[must_use]
            pub fn with_pricing(
                mut self,
                pricing: impl IntoIterator<Item = (impl Into<String>, ::haest_core::catalog::ModelPricing)>,
            ) -> Self {
                let rows = pricing.into_iter().map(|(model, row)| (model.into(), row));
                self.pricing.extend(rows);
                self
            }
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                use $crate::send::{HeaderNames, REDACTED, UrlDebug};
                f.debug_struct(stringify!($name))
                    .field("id", &self.id)
                    .field("api_key", &REDACTED)
                    .field("base_url", &self.base_url.as_deref().map(UrlDebug))
                    .field("headers", &HeaderNames(&self.headers))
                    .field("pricing", &self.pricing)
                    .finish()
            }
        }
    };
}

/// For the provider `$name`, a struct holding its options `$opts` as `opts`
/// and a [`Sender`](super::Sender) as `sender`: `new`, `with_retry`,
/// `with_headers_timeout`, `with_stall_timeout`, `id`, and the
/// [`Provider`](haest_core::Provider)
/// impl that forwards to `id` and its own inherent `stream` and
/// `check_prewarm`.
macro_rules! sender_provider {
    ($name:ident, $opts:ty) => {
        impl $name {
            /// A provider sending over `http` (the client itself, or an `Arc` of one
            /// the host shares) with the default [`RetryPolicy`](crate::RetryPolicy).
            /// Nothing is sent until [`stream`](Self::stream).
            pub fn new(opts: $opts, http: impl Into<::haest_http::HttpClientRef>) -> Self {
                Self {
                    opts,
                    sender: $crate::send::Sender::new(http.into()),
                }
            }

            /// The same provider with `retry` instead of the default policy.
            #[must_use]
            pub fn with_retry(mut self, retry: $crate::RetryPolicy) -> Self {
                self.sender.retry = retry;
                self
            }

            /// The same provider waiting at most `wait` for each try's status and
            /// headers (default ten minutes, as long as the SDKs allow; a long prompt
            /// can sit in a queue before the first byte). A try with no answer by
            /// then counts as unanswered, and the [`RetryPolicy`](crate::RetryPolicy)
            /// may try again; the body then takes as long as it takes.
            #[must_use]
            pub fn with_headers_timeout(mut self, wait: ::std::time::Duration) -> Self {
                self.sender.headers_timeout = wait;
                self
            }

            /// The same provider ending an answer whose stream, once started,
            /// sends nothing for `stall`: the message ends as an error and is
            /// not retried (the text already shown would be sent twice). By
            /// default there is none, and a silent stream waits for the turn's
            /// cap or a cancel.
            #[must_use]
            pub fn with_stall_timeout(mut self, stall: ::std::time::Duration) -> Self {
                self.sender.stall_timeout = Some(stall);
                self
            }

            /// Its id, the options' `id`: what its answers carry as their provider.
            pub fn id(&self) -> &str {
                &self.opts.id
            }
        }

        #[doc = concat!("[`", stringify!($name), "::id`], [`", stringify!($name), "::stream`] and")]
        #[doc = concat!("[`", stringify!($name), "::check_prewarm`].")]
        impl ::haest_core::provider::Provider for $name {
            fn id(&self) -> &str {
                $name::id(self)
            }

            fn stream(
                &self,
                req: ::haest_core::provider::ProviderRequest,
            ) -> ::haest_core::provider::ProviderStream {
                $name::stream(self, req)
            }

            fn check_prewarm(
                &self,
                req: &::haest_core::provider::ProviderRequest,
            ) -> Result<(), ::haest_core::cache::PrewarmRefused> {
                $name::check_prewarm(self, req)
            }
        }
    };
}

pub(crate) use {endpoint_options, sender_provider};
