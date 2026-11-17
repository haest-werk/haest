//! [`ToolError`]: why a tool call failed.

#[cfg(doc)]
use super::Tool;

/// Why a tool call failed. [`Tool::execute`] returns it; the loop turns it
/// into an error result the model reads, and the turn goes on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// A bad call the model can correct; the message goes back verbatim.
    #[error("{0}")]
    Input(String),
    /// Anything else; the message goes back as an error result, prefixed with
    /// the tool's name (`Read failed: ...`).
    #[error("{0}")]
    Failed(String),
}

impl ToolError {
    /// What it says, whatever its kind.
    pub fn message(&self) -> &str {
        match self {
            ToolError::Input(m) | ToolError::Failed(m) => m,
        }
    }

    /// The same kind of error saying what `f` makes of its message: a
    /// wrapper adds a note to every error of the tool it wraps, the kinds
    /// haest adds later included.
    ///
    /// ```
    /// use haest_core::ToolError;
    ///
    /// let e = ToolError::Input("\"path\" is required".into());
    /// let noted = e.map_message(|m| format!("{m} (paths are relative to the notebook)"));
    /// assert!(matches!(noted, ToolError::Input(_)));
    /// assert_eq!(noted.message(), "\"path\" is required (paths are relative to the notebook)");
    /// ```
    #[must_use]
    pub fn map_message(self, f: impl FnOnce(String) -> String) -> Self {
        match self {
            ToolError::Input(m) => ToolError::Input(f(m)),
            ToolError::Failed(m) => ToolError::Failed(f(m)),
        }
    }
}

/// An I/O error is a [`ToolError::Failed`] with the error's text.
impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        ToolError::Failed(e.to_string())
    }
}
