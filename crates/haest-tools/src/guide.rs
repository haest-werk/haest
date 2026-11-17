//! What the system prompt says about the built-in tools a call has: the
//! guide on using them, and whether the workspace's limits matter to it.

/// The built-in tools that work on files by path. The [`tool_guide`]'s path
/// line and [`touches_workspace`] go by them, and a host lists them among
/// the tools a Code program may always call.
pub const FILE_TOOLS: [&str; 5] = ["Read", "Write", "Edit", "Grep", "Glob"];

/// How to use the built-in tools among `tool_names`, one line per kind of
/// tool, for the system prompt's tool guide ([`PromptInputs::tool_guide`](haest_core::context::PromptInputs::tool_guide)).
/// None when the call has none of them.
///
/// ```
/// use haest_tools::tool_guide;
///
/// let guide = tool_guide(&["Read", "WebFetch"]).ok_or("no guide")?;
/// assert!(guide.contains("Read") && !guide.contains("Edit"));
/// assert_eq!(tool_guide(&["mcp__gh__search"]), None);
/// # Ok::<(), &str>(())
/// ```
pub fn tool_guide(tool_names: &[impl AsRef<str>]) -> Option<String> {
    let has = |n: &str| tool_names.iter().any(|t| t.as_ref() == n);
    let look: Vec<&str> = ["Read", "Grep", "Glob"]
        .into_iter()
        .filter(|n| has(n))
        .collect();
    let bash = "Bash for builds, tests, git and anything else";
    let mut lines = Vec::new();
    match (look.is_empty(), has("Bash")) {
        (false, true) => lines.push(format!(
            "- Use {} to look at code; use {bash}.",
            and_list(&look)
        )),
        (false, false) => lines.push(format!("- Use {} to look at code.", and_list(&look))),
        (true, true) => lines.push(format!("- Use {bash}.")),
        (true, false) => {}
    }
    match (has("Edit"), has("Write")) {
        (true, true) => lines.push("- Use Edit for changes to existing files (exact old_string match); Write only for new files or full rewrites.".into()),
        (true, false) => lines.push("- Use Edit for changes to existing files (exact old_string match).".into()),
        (false, true) => lines.push("- Use Write for new files or full rewrites.".into()),
        (false, false) => {}
    }
    if FILE_TOOLS.iter().any(|n| has(n)) {
        lines.push("- Paths may be absolute or relative to the working directory.".into());
    }
    (!lines.is_empty()).then(|| format!("Tool discipline:\n{}", lines.join("\n")))
}

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

/// `[A]` -> "A", `[A, B]` -> "A and B", `[A, B, C]` -> "A, B and C".
fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_covers_only_the_tools_the_call_has() {
        assert_eq!(tool_guide(&[] as &[&str]), None);
        assert_eq!(tool_guide(&["WebFetch", "Skill"]), None);
        let all = ["Read", "Write", "Edit", "Bash", "Grep", "Glob"];
        let guide = tool_guide(&all).unwrap();
        for tool in all {
            assert!(guide.contains(tool), "{tool} is in the guide");
        }
        // Owned names work as well as borrowed ones.
        let read = tool_guide(&[String::from("Read")]).unwrap();
        for absent in ["Edit", "Write", "Bash", "Grep", "Glob"] {
            assert!(!read.contains(absent), "{absent} is not offered");
        }
    }

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
