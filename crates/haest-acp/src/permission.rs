//! [`AcpPrompter`]: haest's approval asks as the client's
//! `session/request_permission`.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};

use agent_client_protocol::v1::schema::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    SessionUpdate, ToolCall as AcpCall, ToolCallContent, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use async_trait::async_trait;
use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter, SHELL_TOOLS};
use haest_session::Hub;
use haest_wire::Level;

use crate::{
    state::{Announced, Relay},
    tell::Teller,
};

/// The option that allows the call.
const ALLOW: &str = "allow";
/// The option that allows it and every later call with the same tool and key.
const ALWAYS: &str = "always";
/// The option that refuses it.
const DENY: &str = "deny";

/// Approval asks put to the client: each is a `session/request_permission`
/// on the tool call it holds up, with allow, allow always and deny.
///
/// haest-mods' gate keeps the rules (an ALWAYS covers the same tool and key;
/// an ask nobody answers is a deny after its timeout). When the gate gives
/// up on an ask (the turn was cancelled, the ask timed out) it drops the
/// prompt's future, and the request is withdrawn with `$/cancel_request`.
pub(crate) struct AcpPrompter {
    /// The session, once it is built: the prompter is made before.
    pub(crate) tell: Arc<OnceLock<Teller>>,
    /// The session's stream, to tell the client what it holds first.
    pub(crate) hub: Hub,
    pub(crate) relay: Relay,
    pub(crate) announced: Announced,
    /// Asks on a call the client was not told of, so far.
    pub(crate) unnamed: AtomicU64,
}

impl AcpPrompter {
    /// The tool call the ask is shown on: the call it holds up when the
    /// client knows it, else one made for the ask (a call of a subagent the
    /// client is not shown).
    fn call_for(&self, tell: &Teller, req: &ApprovalRequest) -> String {
        if let Some(id) = &req.tool_use_id
            && self.announced.contains(id)
        {
            return id.clone();
        }
        let n = self.unnamed.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("approval-{n}");
        let title = match req.input_preview.as_str() {
            "" => req.tool_name.clone(),
            preview => preview.to_string(),
        };
        let kind = if SHELL_TOOLS.contains(&req.tool_name.as_str()) {
            ToolKind::Execute
        } else {
            ToolKind::Other
        };
        let call = AcpCall::new(id.clone(), title)
            .kind(kind)
            .status(ToolCallStatus::Pending);
        tell.tell(SessionUpdate::ToolCall(call));
        id
    }
}

#[async_trait]
impl Prompter for AcpPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
        let Some(tell) = self.tell.get() else {
            return ApprovalAnswer::Deny;
        };
        // Everything the stream holds is told first, so the client has the
        // call this ask holds up.
        self.relay.reached(self.hub.last_seq()).await;
        let id = self.call_for(tell, req);
        let why = ToolCallContent::from(req.description.clone());
        let call = ToolCallUpdate::new(id, ToolCallUpdateFields::new().content(vec![why]));
        let options = vec![
            PermissionOption::new(ALLOW, "Allow", PermissionOptionKind::AllowOnce),
            PermissionOption::new(ALWAYS, "Allow always", PermissionOptionKind::AllowAlways),
            PermissionOption::new(DENY, "Deny", PermissionOptionKind::RejectOnce),
        ];
        let ask = RequestPermissionRequest::new(tell.session.clone(), call, options);
        // Dropped unanswered (the gate gave up), the request is withdrawn.
        let answer = tell.conn.send_request(ask).block_task().await;
        match answer.map(|a| a.outcome) {
            Ok(RequestPermissionOutcome::Selected(picked)) => match &*picked.option_id.0 {
                ALLOW => ApprovalAnswer::Allow,
                ALWAYS => ApprovalAnswer::AllowAlways,
                _ => ApprovalAnswer::Deny,
            },
            // Cancelled (the client's turn was cancelled), an outcome newer
            // than this crate, or no answer: it fails closed.
            _ => ApprovalAnswer::Deny,
        }
    }

    /// An ask an earlier ALWAYS answered: a line in the stream, so the
    /// record of what ran stays complete.
    fn auto_approved(&self, req: &ApprovalRequest) {
        let text = format!("Allowed {} (always): {}", req.tool_name, req.description);
        self.hub.notice(Level::Info, text);
    }
}
