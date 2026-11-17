//! [`Broker`]: the session dialing out to a broker over a WebSocket, so a
//! fleet view elsewhere watches (or drives) it. haest has no broker of its
//! own: this is the agent's end of the link.
//!
//! The link speaks the same wire as every transport, one frame per text
//! message. On each connection:
//!
//! 1. haest sends `hello`, with the token in it (never in the URL, where
//!    proxies and access logs keep it).
//! 2. The broker answers with `events {after, follow: true}`: `after` is the
//!    last seq it holds. That is its ack, and the stream resumes there: a
//!    frame handed to a socket that was already closing is sent again, and
//!    when the ring no longer holds what the broker missed it gets a `gap`.
//!    Nothing is sent before the ack.
//! 3. Requests from the broker are served under the host's [`Grant`]
//!    (watch, unless the host says command).
//!
//! A link that drops is dialed again after a wait with full jitter (a random
//! time up to a cap that doubles each try, [`Backoff`]). A broker that closes
//! with 4401 (the token is wrong) or 4408 (it waited for one in vain), or
//! refuses the handshake with 401 or 403, is not dialed again.

use std::{
    fmt,
    hash::{BuildHasher, Hasher},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use haest_http::__private::{UrlDebug, redacted_url};
use haest_session::BackendRef;
use haest_wire::Token;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{self, Message},
};

use crate::{
    Attached, Ended, Grant, LineSink, LineSource, Start, Transport, TransportError,
    link::until_closed, serve,
};

/// Close codes that mean the broker refused the token: no point dialing again.
pub const AUTH_CLOSE: [u16; 2] = [4401, 4408];

/// How long dialing (TCP, TLS, the handshake) may take.
const DIAL_WAIT: Duration = Duration::from_secs(10);

/// A connection that lasted this long was a good one: the next drop starts
/// the backoff over.
const STABLE: Duration = Duration::from_secs(5);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The session linked to a broker at a `ws://` or `wss://` URL. Its Debug
/// output never shows the token, nor a credential in the URL (its userinfo,
/// a query's values).
///
/// ```no_run
/// # async fn link(session: impl haest_session::Backend + 'static, token: String) -> Result<(), haest_transport::TransportError> {
/// use haest_transport::{Broker, Grant};
/// use haest_wire::Origin;
///
/// // The token comes from the host's keys; it rides in the hello.
/// let link = Broker::new("wss://broker.example/agents").with_token(token);
/// let attached = link.attach(session, Grant::watch(Origin::Agent))?;
/// // Ends when the conversation does, or when the broker refuses the token.
/// let why = attached.ended().await;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Broker {
    url: String,
    token: Option<Token>,
    backoff: Backoff,
}

impl fmt::Debug for Broker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Broker")
            .field("url", &UrlDebug(&self.url))
            .field("token", &self.token)
            .field("backoff", &self.backoff)
            .finish()
    }
}

impl Broker {
    /// A link to the broker at `url` (`ws://` or `wss://`), with no token
    /// and the default [`Backoff`].
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
            backoff: Backoff::default(),
        }
    }

    /// The token the `hello` carries.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<Token>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// How long to wait between dials.
    #[must_use]
    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// The broker's URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The token the `hello` carries, if any.
    pub fn token(&self) -> Option<&Token> {
        self.token.as_ref()
    }

    /// The wait between dials.
    pub fn backoff(&self) -> Backoff {
        self.backoff
    }
}

/// The wait before dialing again: full jitter, a random time between zero
/// and `base` doubled for each failed try, at most `cap`.
///
/// ```
/// use std::time::Duration;
///
/// use haest_transport::Backoff;
///
/// let b = Backoff { base: Duration::from_millis(500), cap: Duration::from_secs(30) };
/// assert_eq!(b.delay(0, 1.0), Duration::from_millis(500));
/// assert_eq!(b.delay(3, 1.0), Duration::from_secs(4));
/// assert_eq!(b.delay(20, 1.0), Duration::from_secs(30));
/// assert_eq!(b.delay(3, 0.25), Duration::from_secs(1));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// The longest first wait.
    pub base: Duration,
    /// The longest wait ever.
    pub cap: Duration,
}

impl Default for Backoff {
    /// Up to half a second at first, never more than 30 seconds.
    fn default() -> Self {
        Self {
            base: Duration::from_millis(500),
            cap: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    /// The wait before try `attempt` (0 after the first failure), at the
    /// point `unit` (0 to 1) of its range.
    pub fn delay(&self, attempt: u32, unit: f64) -> Duration {
        let ceiling = self.base.saturating_mul(1 << attempt.min(31)).min(self.cap);
        ceiling.mul_f64(unit.clamp(0.0, 1.0))
    }
}

/// A number from 0 to 1, different on each call: std's per-instance random
/// hash keys, mixed with the clock.
fn jitter() -> f64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    h.write_u128(now.map_or(0, |d| d.as_nanos()));
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// [`Broker::attach`].
impl Transport for Broker {
    fn attach(
        self,
        backend: impl Into<BackendRef>,
        grant: Grant,
    ) -> Result<Attached, TransportError> {
        Broker::attach(self, backend, grant)
    }
}

impl Broker {
    /// Link `backend` to the broker under `grant`, on a task of its own (in
    /// the current tokio runtime), as [`Transport::attach`] does: it dials,
    /// serves, and dials again after a dropped link until the conversation
    /// ends or the broker refuses the token ([`Attached::ended`] says which).
    ///
    /// # Errors
    ///
    /// [`TransportError::Url`] when the URL is not `ws://` or `wss://`.
    pub fn attach(
        self,
        backend: impl Into<BackendRef>,
        grant: Grant,
    ) -> Result<Attached, TransportError> {
        if !(self.url.starts_with("ws://") || self.url.starts_with("wss://")) {
            return Err(TransportError::Url(redacted_url(&self.url).into_owned()));
        }
        Ok(Attached::new(tokio::spawn(link(
            self,
            backend.into(),
            grant,
        ))))
    }
}

/// Dial, serve, dial again, until the conversation ends or the broker
/// refuses the token.
async fn link(broker: Broker, backend: BackendRef, grant: Grant) -> Ended {
    let closed = until_closed(backend.subscribe(u64::MAX));
    tokio::pin!(closed);
    let mut attempt = 0u32;
    loop {
        let began = Instant::now();
        let dialed = tokio::select! {
            dialed = dial(&broker.url) => dialed,
            () = &mut closed => return Ended::Closed,
        };
        match dialed {
            Ok(ws) => {
                let (sink, stream) = ws.split();
                let start = Start {
                    follow_from: None,
                    token: broker.token.clone(),
                };
                match serve(
                    backend.clone(),
                    grant,
                    start,
                    WsSource(stream),
                    WsSink(sink),
                )
                .await
                {
                    Ended::Closed => return Ended::Closed,
                    Ended::Failed(e @ TransportError::Auth { .. }) => return Ended::Failed(e),
                    Ended::PeerLeft | Ended::Failed(_) => {}
                }
                if began.elapsed() >= STABLE {
                    attempt = 0;
                }
            }
            Err(e @ TransportError::Auth { .. }) => return Ended::Failed(e),
            Err(_) => {}
        }
        let wait = broker.backoff.delay(attempt, jitter());
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = &mut closed => return Ended::Closed,
        }
    }
}

/// One WebSocket to the broker; a handshake refused with 401 or 403 is the
/// token refused.
async fn dial(url: &str) -> Result<Ws, TransportError> {
    let connector = if url.starts_with("wss://") {
        Connector::Rustls(tls()?)
    } else {
        Connector::Plain
    };
    let dialing = connect_async_tls_with_config(url, None, true, Some(connector));
    match tokio::time::timeout(DIAL_WAIT, dialing).await {
        Err(_) => Err(TransportError::WebSocket(
            "the broker did not answer in time".into(),
        )),
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tungstenite::Error::Http(response)))
            if matches!(response.status().as_u16(), 401 | 403) =>
        {
            Err(TransportError::Auth {
                code: response.status().as_u16(),
                reason: response
                    .status()
                    .canonical_reason()
                    .unwrap_or_default()
                    .into(),
            })
        }
        Ok(Err(e)) => Err(TransportError::WebSocket(Box::new(e))),
    }
}

/// TLS on ring, the platform's roots: as the model requests verify.
fn tls() -> Result<Arc<rustls::ClientConfig>, TransportError> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    let made = CONFIG.get_or_init(|| {
        use rustls_platform_verifier::BuilderVerifierExt;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_platform_verifier()
            .map_err(|e| e.to_string())?
            .with_no_client_auth();
        Ok(Arc::new(config))
    });
    made.clone()
        .map_err(|e| TransportError::WebSocket(e.into()))
}

/// The broker's requests: one per text message.
struct WsSource(SplitStream<Ws>);

impl LineSource for WsSource {
    async fn next_line(&mut self) -> Option<Result<String, TransportError>> {
        loop {
            match self.0.next().await? {
                Ok(Message::Text(text)) => return Some(Ok(text.as_str().to_string())),
                Ok(Message::Close(Some(frame))) if AUTH_CLOSE.contains(&u16::from(frame.code)) => {
                    return Some(Err(TransportError::Auth {
                        code: frame.code.into(),
                        reason: frame.reason.as_str().to_string(),
                    }));
                }
                Ok(Message::Close(_)) => return None,
                Ok(_) => continue,
                Err(e) => return Some(Err(TransportError::WebSocket(Box::new(e)))),
            }
        }
    }
}

/// Frames to the broker: one per text message.
struct WsSink(SplitSink<Ws, Message>);

impl LineSink for WsSink {
    async fn send_line(&mut self, line: String) -> Result<(), TransportError> {
        self.0
            .send(Message::Text(line.into()))
            .await
            .map_err(|e| TransportError::WebSocket(Box::new(e)))
    }
}

#[cfg(test)]
mod tests;
