//! [`ToolChangeMessage`]: a change to the tools the model may call, kept in
//! history where it was first sent.

use serde::{Deserialize, Serialize};

use crate::provider::ToolSpec;

/// A change to the tools the model may call, made after a conversation's
/// requests began, kept in history where it was first sent.
///
/// The loop writes one only for a model that takes mid-conversation tool
/// changes ([`CacheFacts::tool_changes`](crate::catalog::CacheFacts::tool_changes):
/// Anthropic's own API). Such a model is sent one `tools` array for the whole
/// conversation, the baseline, and a later change to the agent's tools (a
/// [`LiveTools`](crate::LiveTools) edit, a mod's tool, a tool hidden or shown)
/// reaches it as one of these, right after the newest user or tool message:
/// the request's prefix, tools first, stays as it was and the prompt cache
/// holds. Every other model gets the current tools as its `tools` array, and
/// its provider leaves these out ([`ProviderRequest::tool_baseline`](crate::ProviderRequest::tool_baseline)).
///
/// How the loop places one:
///
/// - It compares the tools a model call would send with the baseline and
///   the changes since, by name and definition, so every change made since
///   the last model call is one message, and a change undone before the next
///   call is none.
/// - It goes right after the newest user or tool message, never first and
///   never between a tool call and its result. A change made after a turn
///   ended waits for the next user message and goes right after it.
/// - It is recorded once the model call it rode in is answered with
///   something; a call that failed records nothing, and the next call sends
///   the change again.
/// - Every later request sends it again as it is: on Anthropic's API an
///   edited or dropped one misses the cache from there on, and on its 5.x
///   models also voids the thinking of every later answer. A session file
///   keeps it, as it keeps every [`Message`](super::Message).
/// - A compaction drops the tool changes among the messages it keeps and
///   starts the baseline over: the next request sends the current tools as
///   the new one.
///
/// The first change after the conversation began, or after a compaction,
/// carries the [`baseline`](Self::baseline), so a resumed conversation sends
/// the same `tools` array again.
///
/// Serialized with `role` `"tool_change"`, a message kind the TypeScript
/// haest does not read: a session file that holds one resumes in this
/// haest only.
///
/// ```
/// use haest_core::{
///     Message,
///     messages::{ToolChange, ToolChangeMessage},
///     provider::ToolSpec,
/// };
/// use serde_json::json;
///
/// let lookup = ToolSpec {
///     name: "Lookup".into(),
///     description: "Look a word up.".into(),
///     parameters: json!({"type": "object"}),
/// };
/// let change = Message::ToolChange(ToolChangeMessage {
///     changes: vec![ToolChange::Removed { name: "Clock".into() }, ToolChange::Added { tool: lookup }],
///     baseline: None,
/// });
/// assert_eq!(
///     serde_json::to_value(&change)?,
///     json!({"role": "tool_change", "changes": [
///         {"type": "removed", "name": "Clock"},
///         {"type": "added", "tool": {"name": "Lookup", "description": "Look a word up.",
///             "parameters": {"type": "object"}}},
///     ]})
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolChangeMessage {
    /// The changes, in the order the model reads them: the tools taken out,
    /// then the tools put in.
    pub changes: Vec<ToolChange>,
    /// The `tools` array the conversation's requests sent until this change,
    /// on the first change since the conversation began or was compacted;
    /// None on every later one. A provider never shows it to the model as a
    /// message: it is what the request's `tools` stay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Vec<ToolSpec>>,
}

/// One change of a [`ToolChangeMessage`]. Serialized with a `type` tag:
/// `"removed"`, `"restored"` or `"added"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolChange {
    /// The tool called `name` is no longer offered.
    Removed {
        /// The tool's name.
        name: String,
    },
    /// A tool of the baseline is offered again, as the baseline defines it:
    /// named, not defined again (Anthropic's `tool_addition` by reference).
    Restored {
        /// The tool's name.
        name: String,
    },
    /// A tool is offered as `tool` defines it: a new one, or a new
    /// definition of the one of that name, which it replaces from here on
    /// (Anthropic's `tool_addition` by value).
    Added {
        /// Its definition.
        tool: ToolSpec,
    },
}
