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
//!    So does an `after` this stream never reached (a broker that kept
//!    another run's frames under the same id): it then resumes from the
//!    stream's last seq. No other frame is sent before the ack.
//! 3. Requests from the broker are served under the host's [`Grant`]
//!    (watch, unless the host says command).
//!
//! A link that drops is dialed again after a wait with full jitter (a random
//! time up to a cap that doubles each try, [`Backoff`]). So is a link the
//! broker goes silent on: haest pings a broker it has not heard from for a
//! while, and takes the path for dead when nothing, not even the pong, comes
//! within the silence limit ([`Broker::with_silence_limit`], 60 s by
//! default). A broker that closes with 4401 (the token is wrong) or 4408 (it
//! waited for one in vain), or refuses the handshake with 401 or 403, is not
//! dialed again. When the conversation ends, haest closes the link with a
//! Close frame, code 1000, so the broker knows it will not be dialed again.

use std::{
    fmt,
    hash::{BuildHasher, Hasher},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use haest_http::{__private::UrlDebug, redacted_url};
use haest_session::BackendRef;
use haest_wire::Token;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        self, Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
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

/// How long the Close frame at the conversation's end may take.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

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
    silence_limit: Option<Duration>,
}

impl fmt::Debug for Broker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Broker")
            .field("url", &UrlDebug(&self.url))
            .field("token", &self.token)
            .field("backoff", &self.backoff)
            .field("silence_limit", &self.silence_limit)
            .finish()
    }
}

impl Broker {
    /// The longest the broker may go unheard by default: see
    /// [`with_silence_limit`](Self::with_silence_limit).
    pub const SILENCE_LIMIT: Duration = Duration::from_secs(60);

    /// A link to the broker at `url` (`ws://` or `wss://`), with no token,
    /// the default [`Backoff`] and the default
    /// [`SILENCE_LIMIT`](Self::SILENCE_LIMIT).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
            backoff: Backoff::default(),
            silence_limit: Some(Self::SILENCE_LIMIT),
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

    /// The longest the broker may go unheard before the link is taken for
    /// dead and dialed again ([`TransportError::Silent`]): a path that went
    /// silent (the broker's host lost power, a NAT forgot the mapping, the
    /// machine slept) sends no close and no reset, so nothing else would
    /// notice it while the conversation is idle. After a third of it with
    /// nothing from the broker, haest pings it, so a live broker always has
    /// something to answer (its WebSocket answers a ping with a pong by
    /// itself). [`SILENCE_LIMIT`](Self::SILENCE_LIMIT), 60 s, by default: a
    /// ping every 20 s of quiet also keeps a proxy that cuts idle links at
    /// 60 s (nginx's and AWS's load balancers' default) from cutting this
    /// one, and a dead link is dialed again within a minute or so. A limit
    /// shorter than the broker's round trip drops a live link (zero drops
    /// every link as soon as it is made, and dials again after the
    /// backoff); one past the clock's range (`Duration::MAX`) is no limit.
    #[must_use]
    pub fn with_silence_limit(mut self, limit: Duration) -> Self {
        self.silence_limit = Some(limit);
        self
    }

    /// No liveness check: no pings, no limit on silence. A path that goes
    /// silent is then noticed only when a write to it fails, which may be
    /// never while the conversation is idle.
    #[must_use]
    pub fn without_silence_limit(mut self) -> Self {
        self.silence_limit = None;
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

    /// The longest the broker may go unheard; None when the link does not
    /// check ([`without_silence_limit`](Self::without_silence_limit)).
    pub fn silence_limit(&self) -> Option<Duration> {
        self.silence_limit
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
                let (heard, limit) = (Heard::now(), broker.silence_limit);
                let source = WsSource {
                    stream,
                    heard: heard.clone(),
                    limit,
                };
                let sink = Arc::new(tokio::sync::Mutex::new(sink));
                let ws_sink = WsSink {
                    sink: sink.clone(),
                    heard,
                    limit,
                    pinged: tokio::time::Instant::now(),
                };
                match serve(backend.clone(), grant, start, source, ws_sink).await {
                    Ended::Closed => {
                        say_ended(&sink).await;
                        return Ended::Closed;
                    }
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

/// The conversation ended: a Close frame (1000) says so, so the broker can
/// tell an agent that finished from a path that died and will be dialed
/// again (RFC 6455 7.1.2). A broker that stopped reading gets a second, no
/// more.
async fn say_ended(sink: &tokio::sync::Mutex<SplitSink<Ws, Message>>) {
    let frame = CloseFrame {
        code: CloseCode::Normal,
        reason: "the conversation ended".into(),
    };
    let close = async { sink.lock().await.send(Message::Close(Some(frame))).await };
    let _ = tokio::time::timeout(CLOSE_WAIT, close).await;
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

/// `wait` after `at`, or None when that is past the clock's range: a silence
/// limit that long (`Duration::MAX`) is no limit, and its ping never due.
fn after(at: tokio::time::Instant, wait: Duration) -> Option<tokio::time::Instant> {
    at.checked_add(wait)
}

/// When the broker was last heard from (any frame, a pong too), shared by
/// the link's two halves: the source reads by it, the sink pings by it.
#[derive(Clone)]
struct Heard(Arc<Mutex<tokio::time::Instant>>);

impl Heard {
    fn now() -> Self {
        Self(Arc::new(Mutex::new(tokio::time::Instant::now())))
    }

    fn at(&self) -> tokio::time::Instant {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn mark(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = tokio::time::Instant::now();
    }
}

/// The broker's requests: one per text message. With a silence limit, a
/// broker unheard for that long ends the link ([`TransportError::Silent`]).
struct WsSource {
    stream: SplitStream<Ws>,
    heard: Heard,
    limit: Option<Duration>,
}

impl WsSource {
    /// The next message; Err when the broker has been silent past the limit
    /// (measured from the last frame heard, so a dropped call loses nothing).
    async fn next_message(
        &mut self,
    ) -> Result<Option<Result<Message, tungstenite::Error>>, TransportError> {
        let Some(limit) = self.limit else {
            return Ok(self.stream.next().await);
        };
        let Some(deadline) = after(self.heard.at(), limit) else {
            return Ok(self.stream.next().await);
        };
        tokio::time::timeout_at(deadline, self.stream.next())
            .await
            .map_err(|_elapsed| TransportError::Silent(limit))
    }
}

impl LineSource for WsSource {
    async fn next_line(&mut self) -> Option<Result<String, TransportError>> {
        loop {
            let message = match self.next_message().await {
                Ok(message) => message?,
                Err(silent) => return Some(Err(silent)),
            };
            if message.is_ok() {
                self.heard.mark();
            }
            match message {
                Ok(Message::Text(text)) => return Some(Ok(text.as_str().to_string())),
                Ok(Message::Close(Some(frame))) if AUTH_CLOSE.contains(&u16::from(frame.code)) => {
                    return Some(Err(TransportError::Auth {
                        code: frame.code.into(),
                        reason: frame.reason.as_str().to_string(),
                    }));
                }
                Ok(Message::Close(_)) => return None,
                // A ping, a pong, a binary message: read on.
                Ok(_) => {}
                Err(e) => return Some(Err(TransportError::WebSocket(Box::new(e)))),
            }
        }
    }
}

/// Frames to the broker: one per text message. With a silence limit, a
/// ping once the broker has been quiet for a third of it.
struct WsSink {
    /// Shared with the link, which closes it once the conversation ended.
    sink: Arc<tokio::sync::Mutex<SplitSink<Ws, Message>>>,
    heard: Heard,
    limit: Option<Duration>,
    /// The last ping sent: the next waits a third of the limit after it.
    pinged: tokio::time::Instant,
}

impl WsSink {
    async fn send(&self, message: Message) -> Result<(), TransportError> {
        (self.sink.lock().await.send(message).await)
            .map_err(|e| TransportError::WebSocket(Box::new(e)))
    }
}

impl LineSink for WsSink {
    async fn send_line(&mut self, line: String) -> Result<(), TransportError> {
        self.send(Message::Text(line.into())).await
    }

    async fn keepalive(&mut self) -> Result<(), TransportError> {
        let Some(limit) = self.limit else {
            return std::future::pending().await;
        };
        // A third of the limit: the broker has two pings to answer before it.
        let quiet = (limit / 3).max(Duration::from_millis(1));
        loop {
            // Heard from since the wait began: wait again, from then.
            let Some(due) = after(self.heard.at().max(self.pinged), quiet) else {
                return std::future::pending().await;
            };
            if tokio::time::Instant::now() >= due {
                break;
            }
            tokio::time::sleep_until(due).await;
        }
        self.pinged = tokio::time::Instant::now();
        self.send(Message::Ping(tungstenite::Bytes::new())).await
    }
}

#[cfg(test)]
mod tests;
