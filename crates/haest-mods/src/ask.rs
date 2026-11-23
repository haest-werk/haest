//! A mod's ask for the human's Allow / Deny ([`ApprovalRequest`]), and the
//! answer ([`ApprovalDecision`]).

use serde::{Deserialize, Serialize};

/// An Allow / Deny prompt for the human, raised by a mod through
/// [`ModUi::ask`](crate::ModUi::ask).
///
/// A mod file builds one as the `req` of `haest.ask(req, signal)`, in
/// camelCase: `{ toolName, description, inputPreview, toolUseId?, key? }`.
/// More may come: a native mod builds one with [`new`](Self::new) and a
/// `with_*` setter per further part, the preview included, so the command
/// and the question can never be swapped.
///
/// ```
/// use haest_mods::ApprovalRequest;
///
/// let req = ApprovalRequest::new("Bash", "haest wants to delete files")
///     .with_input_preview("rm -rf build")
///     .with_key("rm -rf build");
/// assert_eq!(req.input_preview, "rm -rf build");
/// assert_eq!(req.key.as_deref(), Some("rm -rf build"));
/// assert_eq!(req.tool_use_id, None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ApprovalRequest {
    /// The tool the ask is about (`Bash`); [`ApprovalGate`](crate::ApprovalGate) scopes ALWAYS to it.
    pub tool_name: String,
    /// The question, in one sentence: "haest wants to run a command that deletes files recursively".
    pub description: String,
    /// What the human reads to decide: a shell command whole (it is what
    /// runs; a cut could hide its tail), else the start of the input. A
    /// prompter shows it as text: control characters escaped, never drawn.
    /// Empty when there is nothing to show but the question.
    pub input_preview: String,
    /// The tool call this ask holds up, when the mod knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// What the human's ALWAYS on this ask covers (asks with the same tool and key
    /// answer themselves); None = the description. A Bash ask uses the exact command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl ApprovalRequest {
    /// An ask about `tool_name`, saying `description`: no preview, tied to
    /// no tool call, and its ALWAYS keyed on the description.
    #[must_use]
    pub fn new(tool_name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            description: description.into(),
            input_preview: String::new(),
            tool_use_id: None,
            key: None,
        }
    }

    /// What the human reads to decide: the command whole, else the start
    /// of the input.
    #[must_use]
    pub fn with_input_preview(mut self, preview: impl Into<String>) -> Self {
        self.input_preview = preview.into();
        self
    }

    /// The tool call this ask holds up.
    #[must_use]
    pub fn with_tool_use_id(mut self, id: impl Into<String>) -> Self {
        self.tool_use_id = Some(id.into());
        self
    }

    /// What the human's ALWAYS covers, in place of the description (a Bash
    /// ask uses the exact command).
    #[must_use]
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

haest_core::word_enum! {
    /// The human's answer to one ask: what [`ModUi::ask`](crate::ModUi::ask) returns. Its word
    /// (`"allow"`, `"deny"`) is what a mod file's `haest.ask` promise
    /// resolves to.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ApprovalDecision ("decision") {
        /// The human allowed it.
        Allow = "allow",
        /// The human refused it, did not answer in time, or the call was interrupted.
        Deny = "deny",
    }
}
