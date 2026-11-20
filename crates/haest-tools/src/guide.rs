//! Which built-in tools work on the workspace, and so when the workspace's
//! limits belong in the system prompt. Each tool's own line for the prompt's
//! tool guide is its [`Tool::prompt_guide`](haest_core::Tool::prompt_guide).

/// The built-in tools that work on files by path. [`touches_workspace`]
/// goes by them, and a host lists them among the tools a Code program may
/// always call.
pub const FILE_TOOLS: [&str; 5] = ["Read", "Write", "Edit", "Grep", "Glob"];

/// Whether a call offered `tool_names` can touch the workspace (Bash or a
/// file tool): only then does the workspace's own note
/// ([`Workspace::prompt_note`](haest_core::Workspace::prompt_note), a
/// sandbox's limits) belong in its prompt.
///
/// ```
/// use haest_tools::touches_workspace;
///
/// assert!(touches_workspace(&["WebFetch", "Edit"]));
/// assert!(!touches_workspace(&["WebFetch"]));
/// ```
pub fn touches_workspace(tool_names: &[impl AsRef<str>]) -> bool {
    tool_names
        .iter()
        .map(AsRef::as_ref)
        .any(|t| t == "Bash" || FILE_TOOLS.contains(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_and_the_file_tools_touch_the_workspace_and_nothing_else_does() {
        for touches in [&["Bash"][..], &["Glob"], &["WebFetch", "Edit"]] {
            assert!(touches_workspace(touches), "{touches:?}");
        }
        for not in [&[][..], &["WebFetch", "mcp__gh__search"]] {
            assert!(!touches_workspace(not), "{not:?}");
        }
    }
}
