//! [`HttpHook`]: a hook behind a URL, posted the event once per event, its
//! response the decision.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use haest_core::{error::BoxError, workspace::MAX_OUTPUT_BYTES};
use haest_http::{HttpClientRef, Request, redacted_url};
use haest_mods::{Event, Hook};
use tokio_util::sync::CancellationToken;

use crate::dialect::{Dialect, Haest, HookFailure, Reply, outgoing, shown};

#[cfg(test)]
mod tests;

/// The most of an answer read: as much as a command keeps of its stdout
/// (the shell bridge's). Past it, the answer is a failure.
const MAX_ANSWER: usize = MAX_OUTPUT_BYTES;

/// How much of an error status's body is read for its report.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// A hook behind a URL: each event is a POST of its JSON, and the 2xx
/// response's body is the decision. Bind it as any
/// [`Hook`](haest_mods::Hook), with the timeout, `on_error` and tool
/// matcher of its [`Binding`](haest_mods::Binding).
///
/// It speaks a dialect, as [`ShellHook`](crate::ShellHook) does: [`Haest`]
/// (the default, every event) or [`Claude`](crate::Claude) (the JSON Claude
/// Code's HTTP hooks get and answer, the same as its command hooks';
/// [`with_dialect`](Self::with_dialect)). A `HttpHook<Claude>` is a hook only
/// of the events Claude Code has, `tool.before`, `tool.after`,
/// `prompt.submit`, `system.prompt` and `turn.end`, as
/// [`ModEvent::claude_code`](haest_mods::ModEvent::claude_code) lists them,
/// and is told the session each event's payload names:
/// [`HookSet`](haest_mods::HookSet) sets it from the conversation's
/// [`ModEnv`](haest_mods::ModEnv), and a host that calls
/// [`Hook::run`](haest_mods::Hook::run) itself sets the payload's `session`
/// first (from `haest.session()`).
/// [docs/hooks.md](https://github.com/haest-werk/haest/blob/main/docs/hooks.md)
/// shows every event's body and what each answer becomes, in both dialects.
///
/// | Response | What it is |
/// |---|---|
/// | 2xx, empty body | a pass |
/// | 2xx, the event's decision in the dialect | that decision |
/// | 2xx, anything else | a failure |
/// | any other status, no answer, a body past 64 MiB | a failure, its status and body in the report |
///
/// A failure and a timeout do what the binding's
/// [`OnError`](haest_mods::OnError) says (by default `tool.before` refuses
/// the call, every other event goes on). An HTTP hook has no exit 2: a
/// block is an answer. The request goes through the host's
/// [`HttpClient`](haest_http::HttpClient) ([`with_client`](Self::with_client);
/// the default is haest-http's, which sends only with haest-http's `reqwest`
/// feature on), with `content-type: application/json` and the headers set
/// here. An interrupt drops it.
///
/// An HTTP hook receives every tool input it is bound to: give one only
/// from the human's own configuration, never a project's.
///
/// # Examples
///
/// A log of every finished turn, and a reviewer service that may keep a
/// turn going, with a token:
///
/// ```
/// use std::time::Duration;
///
/// use haest_bridges::HttpHook;
/// use haest_mods::{Binding, HookSet, OnError, TurnEndPayload};
///
/// let review = HttpHook::new("https://review.internal/haest/turn-end")
///     .with_header("authorization", "Bearer t0k3n");
/// let hooks = HookSet::new().on_turn_end(
///     Binding::<TurnEndPayload>::new(review)
///         .with_timeout(Duration::from_secs(10))
///         .with_on_error(OnError::Continue),
/// );
/// ```
#[derive(Clone)]
pub struct HttpHook<D = Haest> {
    url: String,
    dialect: D,
    headers: Vec<(String, String)>,
    client: HttpClientRef,
    cwd: Option<PathBuf>,
}

/// The URL without its credentials, each header's name without its value.
impl<D: fmt::Debug> fmt::Debug for HttpHook<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("HttpHook")
            .field("url", &redacted_url(&self.url))
            .field("dialect", &self.dialect)
            .field("headers", &headers)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl HttpHook {
    /// A POST to `url` per event, in haest's dialect, through haest-http's
    /// default client.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            dialect: Haest,
            headers: Vec::new(),
            client: HttpClientRef::default(),
            cwd: None,
        }
    }
}

impl<D> HttpHook<D> {
    /// The same URL, speaking `dialect`: [`Haest`], or
    /// [`Claude`](crate::Claude) for a Claude Code HTTP hook.
    pub fn with_dialect<T>(self, dialect: T) -> HttpHook<T> {
        HttpHook {
            url: self.url,
            dialect,
            headers: self.headers,
            client: self.client,
            cwd: self.cwd,
        }
    }

    /// Send `name: value` with every request (a token). A header of the
    /// same name, in any case, is replaced.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        self.headers
            .retain(|(had, _)| !had.eq_ignore_ascii_case(&name));
        self.headers.push((name, value.into()));
        self
    }

    /// Send through `client`, the host's own (its gateway, its proxy, its
    /// test's fake).
    #[must_use]
    pub fn with_client(mut self, client: impl Into<HttpClientRef>) -> Self {
        self.client = client.into();
        self
    }

    /// The conversation's directory, which [`Claude`](crate::Claude) names
    /// as `cwd`: else a tool event's own, else the host process's.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// The URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The conversation's directory when the host set one.
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// The dialect it speaks.
    pub fn dialect(&self) -> &D {
        &self.dialect
    }

    /// `payload` through the server: its decision, or why there is none.
    async fn ask<E: Event>(
        &self,
        payload: E,
        cancel: &CancellationToken,
    ) -> Result<E::Decision, HookFailure>
    where
        D: Dialect<E>,
    {
        let out = outgoing(&self.dialect, &payload, self.cwd.as_deref())?;
        let mut req = Request::post(&self.url).with_header("content-type", "application/json");
        for (name, value) in &self.headers {
            req = req.with_header(name, value);
        }
        let answered = async {
            let res = self
                .client
                .send(req.with_body(out.body))
                .await
                .map_err(HookFailure::Http)?;
            if !res.is_success() {
                let status = res.status;
                let body = res.body.bytes(MAX_ERROR_BODY).await.unwrap_or_default();
                let body = shown(&String::from_utf8_lossy(&body));
                return Err(HookFailure::Status { status, body });
            }
            let body = res
                .body
                .bytes(MAX_ANSWER)
                .await
                .map_err(HookFailure::Http)?;
            Ok(String::from_utf8_lossy(&body).into_owned())
        };
        let text = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HookFailure::Interrupted),
            text = answered => text?,
        };
        self.dialect.decide(payload, Reply::Answer(text))
    }
}

/// A hook of every event its dialect speaks: every one in [`Haest`], the
/// ones Claude Code has in [`Claude`](crate::Claude).
#[async_trait]
impl<E: Event, D: Dialect<E>> Hook<E> for HttpHook<D> {
    async fn run(&self, payload: E, cancel: &CancellationToken) -> Result<E::Decision, BoxError> {
        Ok(self.ask(payload, cancel).await?)
    }
}
