//! rmcp's streamable HTTP over haest's reqwest client, with every error's URL
//! scrubbed. reqwest puts the request's URL, query included, in each error
//! it makes, and an MCP server's URL may carry its key (`?api_key=...`).
//! rmcp hands that error up whole: into a failure to start, the hub's log, a
//! tool call's answer to the model. Here, before rmcp sees it, the URL loses
//! its query, fragment and userinfo, so nothing above holds the key.

use std::{collections::HashMap, sync::Arc};

use futures::stream::BoxStream;
use haest_http::{
    __private::without_credentials,
    reqwest::{
        self,
        header::{HeaderName, HeaderValue},
    },
};
use rmcp::{
    model::ClientJsonRpcMessage,
    transport::streamable_http_client::{
        SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
    },
};
use sse_stream::Sse;

/// haest's reqwest client as rmcp's HTTP client, each error's URL scrubbed
/// ([`without_credentials`]).
///
/// An SSE stream's own errors pass as they are: reqwest makes a body's
/// error without a URL.
#[derive(Clone, Debug)]
pub(crate) struct ScrubbedClient(pub(crate) reqwest::Client);

type Failed = StreamableHttpError<reqwest::Error>;

type Events = BoxStream<'static, Result<Sse, SseError>>;

/// `e` with its reqwest error's URL scrubbed; the other kinds hold no URL.
fn scrubbed(e: Failed) -> Failed {
    match e {
        StreamableHttpError::Client(e) => StreamableHttpError::Client(without_credentials(e)),
        other => other,
    }
}

/// Each method is reqwest's, its error scrubbed.
#[warn(clippy::missing_trait_methods)]
impl StreamableHttpClient for ScrubbedClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, Failed> {
        self.0
            .post_message(uri, message, session_id, auth_header, custom_headers)
            .await
            .map_err(scrubbed)
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, Failed> {
        self.0
            .post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await
            .map_err(scrubbed)
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), Failed> {
        self.0
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
            .map_err(scrubbed)
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<Events, Failed> {
        self.0
            .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
            .await
            .map_err(scrubbed)
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<Events, Failed> {
        self.0
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await
            .map_err(scrubbed)
    }
}
