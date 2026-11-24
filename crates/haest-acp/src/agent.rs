//! [`AcpAgent`]: haest serving one ACP client, every request it sends.

use std::{path::PathBuf, sync::Arc};

use agent_client_protocol::{
    Agent, BoxFuture, ByteStreams, Client, Error, JsonRpcResponse, Responder, V1,
    on_receive_notification, on_receive_request,
    schema::ProtocolVersion,
    v1::ConnectionTo,
    v1::schema::{
        AgentCapabilities, CancelNotification, CloseSessionRequest, CloseSessionResponse,
        Implementation, InitializeRequest, InitializeResponse, ListSessionsRequest,
        LoadSessionRequest, LoadSessionResponse, McpCapabilities, NewSessionRequest,
        NewSessionResponse, PromptCapabilities, PromptRequest, ResumeSessionRequest,
        ResumeSessionResponse, SessionCapabilities, SessionCloseCapabilities,
        SessionListCapabilities, SessionResumeCapabilities, SetSessionConfigOptionRequest,
        SetSessionModeRequest,
    },
};
use haest_runtime::Resume;
use haest_session::Control;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::{
    AcpError, ConversationsRef,
    open::{Opened, Request, open},
    prompt, settings,
    state::{ClientCaps, State, no_session},
};

/// haest as an ACP agent: serves one client (an editor that started haest)
/// over a pair of byte streams, every session it opens a whole haest
/// conversation built from what [`Conversations`](crate::Conversations)
/// hands in.
///
/// ```no_run
/// use haest_acp::{AcpAgent, Conversations, Setup};
/// use haest_core::error::BoxError;
/// use haest_runtime::RuntimeInputs;
///
/// struct Host;
///
/// #[async_trait::async_trait]
/// impl Conversations for Host {
///     async fn inputs(&self, setup: &Setup) -> Result<RuntimeInputs, BoxError> {
///         // The host's settings, keys and providers for `setup.cwd`.
///         # unreachable!()
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_acp::AcpError> {
/// // The editor started this process: it speaks ACP on stdin and stdout.
/// AcpAgent::new(Host, "/work").serve_stdio().await
/// # }
/// ```
#[derive(Debug)]
pub struct AcpAgent {
    conversations: ConversationsRef,
    cwd: PathBuf,
    name: String,
    version: String,
}

impl AcpAgent {
    /// An agent whose conversations `conversations` builds. `cwd` is where a
    /// request that names no directory looks (`session/list` without one).
    pub fn new(conversations: impl Into<ConversationsRef>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            conversations: conversations.into(),
            cwd: cwd.into(),
            name: "haest".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    /// The name the client is told the agent has; `haest` by default.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The version the client is told; haest-acp's by default.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Serve the client that writes to `reader` and reads from `writer`
    /// until it closes the connection; then every session still open is
    /// closed (its conversation ends, its file is saved).
    ///
    /// Each handler answers at once or hands its work to a task of the
    /// connection's, so a long request (a session built, a turn run) never
    /// holds up the next message. Needs a tokio runtime: the conversations
    /// run on it.
    ///
    /// # Errors
    ///
    /// [`AcpError::Connection`] when the connection fails.
    pub async fn serve<R, W>(self, reader: R, writer: W) -> Result<(), AcpError>
    where
        R: AsyncRead + Send + 'static,
        W: AsyncWrite + Send + 'static,
    {
        let info = Implementation::new(self.name, self.version);
        let state = Arc::new(State::new(self.conversations, self.cwd));
        let transport = ByteStreams::new(writer.compat_write(), reader.compat());
        let s = [(); 9].map(|()| state.clone());
        let [s0, s1, s2, s3, s4, s5, s6, s7, s8] = s;
        let served = Agent
            .v1()
            .name("haest")
            .on_receive_request(
                async move |req: InitializeRequest, responder, _cx| {
                    s0.set_caps(ClientCaps::of(&req.client_capabilities));
                    let answer = InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(capabilities())
                        .agent_info(info.clone());
                    responder.respond(answer)
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |req: NewSessionRequest, responder, cx| {
                    let (state, conn) = (s1.clone(), cx.clone());
                    let req = Request {
                        cwd: req.cwd,
                        mcp_servers: req.mcp_servers,
                        resume: Resume::New,
                        replay: false,
                    };
                    cx.spawn(answer_open(state, conn, req, responder, |o| {
                        NewSessionResponse::new(o.live.id.clone())
                            .modes(o.modes.clone())
                            .config_options(o.options.clone())
                    }))
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |req: LoadSessionRequest, responder, cx| {
                    let (state, conn) = (s2.clone(), cx.clone());
                    let req = Request {
                        cwd: req.cwd,
                        mcp_servers: req.mcp_servers,
                        resume: Resume::Id(req.session_id.0.to_string()),
                        replay: true,
                    };
                    cx.spawn(answer_open(state, conn, req, responder, |o| {
                        LoadSessionResponse::new()
                            .modes(o.modes.clone())
                            .config_options(o.options.clone())
                    }))
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |req: ResumeSessionRequest, responder, cx| {
                    let (state, conn) = (s3.clone(), cx.clone());
                    let req = Request {
                        cwd: req.cwd,
                        mcp_servers: req.mcp_servers,
                        resume: Resume::Id(req.session_id.0.to_string()),
                        replay: false,
                    };
                    cx.spawn(answer_open(state, conn, req, responder, |o| {
                        ResumeSessionResponse::new()
                            .modes(o.modes.clone())
                            .config_options(o.options.clone())
                    }))
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |req: PromptRequest, responder, cx| {
                    let live = match s4.session(&req.session_id) {
                        Ok(live) => live,
                        Err(e) => return responder.respond_with_error(e),
                    };
                    let cancelled = responder.cancellation();
                    cx.spawn(async move {
                        let ended = prompt::run(live, &req.prompt, cancelled).await;
                        responder.respond_with_result(ended)
                    })
                },
                on_receive_request!(),
            )
            .on_receive_notification(
                async move |cancel: CancelNotification, cx| {
                    // A session not open here has no turn to stop.
                    let Ok(live) = s5.session(&cancel.session_id) else {
                        return Ok(());
                    };
                    cx.spawn(async move {
                        let _ = live.session.control(Control::Interrupt).await;
                        Ok(())
                    })
                },
                on_receive_notification!(),
            )
            .on_receive_request(
                async move |req: SetSessionModeRequest, responder, cx| {
                    let live = match s6.session(&req.session_id) {
                        Ok(live) => live,
                        Err(e) => return responder.respond_with_error(e),
                    };
                    cx.spawn(async move {
                        responder.respond_with_result(settings::set_mode(&live, &req.mode_id).await)
                    })
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |req: SetSessionConfigOptionRequest, responder, cx| {
                    let live = match s7.session(&req.session_id) {
                        Ok(live) => live,
                        Err(e) => return responder.respond_with_error(e),
                    };
                    cx.spawn(async move {
                        let set = settings::set_option(&live, &req.config_id, &req.value).await;
                        responder.respond_with_result(set)
                    })
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async move |req: CloseSessionRequest, responder, cx| {
                    let Some(live) = s8.remove(&req.session_id) else {
                        return responder.respond_with_error(no_session(&req.session_id));
                    };
                    cx.spawn(async move {
                        live.close().await;
                        responder.respond(CloseSessionResponse::new())
                    })
                },
                on_receive_request!(),
            )
            .on_receive_request(
                {
                    let state = state.clone();
                    async move |req: ListSessionsRequest, responder, cx| {
                        let state = state.clone();
                        let listed: BoxFuture<'static, Result<(), Error>> = Box::pin(async move {
                            responder.respond_with_result(settings::list(&state, req.cwd).await)
                        });
                        cx.spawn(listed)
                    }
                },
                on_receive_request!(),
            )
            .connect_to(transport)
            .await;
        state.close_all().await;
        served.map_err(|e| AcpError::Connection(e.into()))
    }

    /// [`serve`](Self::serve) over this process's stdin and stdout: how an
    /// editor that started haest talks to it. Nothing else may write to
    /// stdout meanwhile.
    ///
    /// # Errors
    ///
    /// [`AcpError::Connection`] when the connection fails.
    pub async fn serve_stdio(self) -> Result<(), AcpError> {
        self.serve(tokio::io::stdin(), tokio::io::stdout()).await
    }
}

/// Open the session `req` asks for, and answer with what `answer` makes of
/// it; then tell the client the session's commands and stream.
///
/// Boxed where its type is known: the connection's task takes a `Send`
/// future, and the compiler cannot prove an unboxed one of these is
/// (rust-lang/rust#100013).
fn answer_open<T: JsonRpcResponse>(
    state: Arc<State>,
    conn: ConnectionTo<Client>,
    req: Request,
    responder: Responder<T, V1>,
    answer: impl FnOnce(&Opened) -> T + Send + 'static,
) -> BoxFuture<'static, Result<(), Error>> {
    Box::pin(async move {
        let opened = match open(&state, conn, req).await {
            Ok(opened) => opened,
            Err(e) => return responder.respond_with_error(e),
        };
        state.insert(opened.live.clone());
        responder.respond(answer(&opened))?;
        opened.go();
        Ok(())
    })
}

/// What haest says it takes: loading, listing, resuming and closing saved
/// sessions; text and embedded text in a prompt; MCP servers over stdio and
/// HTTP.
fn capabilities() -> AgentCapabilities {
    AgentCapabilities::new()
        .load_session(true)
        .prompt_capabilities(PromptCapabilities::new().embedded_context(true))
        .mcp_capabilities(McpCapabilities::new().http(true))
        .session_capabilities(
            SessionCapabilities::new()
                .list(SessionListCapabilities::new())
                .resume(SessionResumeCapabilities::new())
                .close(SessionCloseCapabilities::new()),
        )
}
