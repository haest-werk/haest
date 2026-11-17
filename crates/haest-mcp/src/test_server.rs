//! A real MCP server for tests, on the other end of an in-memory duplex: no
//! process, no network. Tools: `echo` (text, read-only), `shot` (an image),
//! `fail` (an error result), `reject` (a JSON-RPC error), `hang` (answers only by
//! noticing it was cancelled), plus one tool per entry of `names`, answering
//! `<label>:<its name>` (which server and which tool a call reached).

use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, InitializeResult,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool, ToolAnnotations,
    },
    service::RequestContext,
};
use serde_json::{Value, json};
use tokio::{io::DuplexStream, sync::Notify, task::JoinHandle};

#[derive(Clone, Default)]
pub(crate) struct TestServer {
    /// Signalled when a `hang` call arrives.
    pub hanging: Arc<Notify>,
    /// Signalled when a `hang` call is cancelled by the client.
    pub cancelled: Arc<Notify>,
    /// Names the answers of the `names` tools.
    pub label: String,
    /// Extra tools, listed after the fixed ones under exactly these names.
    pub names: Vec<String>,
}

impl TestServer {
    /// A server labelled `label` with these extra tools.
    pub(crate) fn named(label: &str, names: &[&str]) -> Self {
        Self {
            label: label.into(),
            names: names.iter().map(|n| n.to_string()).collect(),
            ..Self::default()
        }
    }
}

fn schema(v: Value) -> Arc<serde_json::Map<String, Value>> {
    Arc::new(v.as_object().unwrap().clone())
}

impl ServerHandler for TestServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let text = json!({"type": "object", "properties": {"text": {"type": "string"}}});
        let empty = json!({"type": "object", "properties": {}});
        let mut tools = vec![
            Tool::new("echo", "Echo text back", schema(text))
                .with_annotations(ToolAnnotations::new().read_only(true)),
            Tool::new_with_raw("shot", None, schema(empty.clone())),
            Tool::new("fail", "Always fails", schema(empty.clone())),
            Tool::new(
                "reject",
                "Answers with a JSON-RPC error",
                schema(empty.clone()),
            ),
            Tool::new("hang", "Never answers", schema(empty.clone())),
        ];
        tools.extend(
            self.names
                .iter()
                .map(|n| Tool::new(n.clone(), "Answers with its name", schema(empty.clone()))),
        );
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let result = match request.name.as_ref() {
            "echo" => {
                let text = args.get("text").and_then(Value::as_str).unwrap_or("");
                CallToolResult::success(vec![ContentBlock::text(text)])
            }
            "shot" => CallToolResult::success(vec![
                ContentBlock::text("screenshot taken"),
                ContentBlock::image("AQID", "image/png"),
            ]),
            "fail" => CallToolResult::error(vec![ContentBlock::text("boom")]),
            "hang" => {
                self.hanging.notify_one();
                context.ct.cancelled().await;
                self.cancelled.notify_one();
                return Err(ErrorData::internal_error("cancelled", None));
            }
            named if self.names.iter().any(|n| n == named) => {
                CallToolResult::success(vec![ContentBlock::text(format!("{}:{named}", self.label))])
            }
            other => {
                return Err(ErrorData::invalid_params(format!("refused {other}"), None));
            }
        };
        Ok(result.into())
    }
}

/// The server running on one end of a duplex; the other end is for the client.
/// The handle finishes when the server's connection ends.
pub(crate) fn spawn(server: TestServer) -> (DuplexStream, JoinHandle<()>) {
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    let handle = tokio::spawn(async move {
        if let Ok(running) = server.serve(server_end).await {
            let _ = running.waiting().await;
        }
    });
    (client_end, handle)
}
