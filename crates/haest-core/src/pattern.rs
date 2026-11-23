//! The one name pattern haest's settings take: an exact name, or a prefix
//! ending in `*` (`mcp__github__*`, `anthropic:*`). [`name_matches`] reads
//! one, [`check`] says whether a text is one.

/// Whether `name` matches `pattern`: `prefix*` matches every name that
/// starts with `prefix` (a lone `*` matches all), anything else only itself.
pub fn name_matches(name: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

/// What a pattern's names are, for [`check`]: a tool's name never holds a
/// `.`, a model spec often does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternOf {
    /// Tool names: `code_tools`, a hook's `matcher`. No tool's name holds a
    /// `.` (an MCP tool's dots become `_`), so a pattern ending in `.*` is
    /// Claude Code's regular expression (`Bash.*`, `mcp__memory__.*`) and
    /// would match no tool.
    Tools,
    /// Model specs: the `[cache.models."<pattern>"]` keys. A spec holds dots
    /// (`gpt-6.1-sol`), so `openrouter:openai/gpt-6.*` is a prefix like any
    /// other: every `gpt-6.1-*` spec, and not `gpt-6-luna`.
    Models,
}

/// Why a text is not a pattern ([`check`]): read by [`name_matches`], it
/// would match no name, so a setting holding it would quietly do nothing.
/// Each setting says it in its own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BadPattern {
    /// The empty text.
    #[error("an empty pattern matches nothing")]
    Empty,
    /// A regular expression (`|`, `(`, `[`, `{`, `^`, `$`, `+`, `?`, `\`;
    /// for a tool's name, a `.*` end), or a `*` that is not the last
    /// character: haest reads no regular expression, and `*` only as the
    /// end of a prefix.
    #[error(
        "not haest's pattern: an exact name, or a prefix ending in `*`; a regular expression is not read, and `*` only ends a prefix"
    )]
    NotAPattern,
    /// A `,` or white space: no tool's name and no model spec holds one, so
    /// the text is a list (`Edit,Write`, `Edit Write`), and a pattern is one
    /// name or one prefix.
    #[error("a pattern is one name or one prefix, not a list: no name holds a `,` or a space")]
    List,
}

/// Whether `pattern` is haest's pattern for `of`: not empty, at most one `*`
/// and only at its end, no `,` or white space, nothing a regular
/// expression would mean. Claude Code's matchers are regular expressions
/// (`Edit|Write`, `Bash.*`), and a file copied from it must fail to load
/// rather than match nothing.
///
/// A `.*` end is a regular expression only in a tool's name
/// ([`PatternOf::Tools`]); in a model spec it is the prefix up to the dot
/// ([`PatternOf::Models`]).
///
/// # Errors
///
/// [`BadPattern`] when it is not one: empty, not a name or a prefix, or a
/// list.
pub fn check(pattern: &str, of: PatternOf) -> Result<(), BadPattern> {
    const REGEX: &[char] = &['|', '(', ')', '[', ']', '{', '}', '^', '$', '+', '?', '\\'];
    if pattern.is_empty() {
        return Err(BadPattern::Empty);
    }
    let star_inside = (pattern.find('*')).is_some_and(|at| at + 1 != pattern.len());
    let dot_star = of == PatternOf::Tools && pattern.ends_with(".*");
    if pattern.contains(REGEX) || star_inside || dot_star {
        return Err(BadPattern::NotAPattern);
    }
    if pattern.contains(|c: char| c == ',' || c.is_whitespace()) {
        return Err(BadPattern::List);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BadPattern, PatternOf, check, name_matches};

    #[test]
    fn an_exact_pattern_matches_only_itself_and_a_star_matches_its_prefix() {
        assert!(name_matches("Bash", "Bash"));
        assert!(!name_matches("BashOutput", "Bash"));
        assert!(name_matches("mcp__github__search", "mcp__github__*"));
        assert!(!name_matches("mcp__gitlab__search", "mcp__github__*"));
        assert!(name_matches("anthropic:claude-opus-5-5", "anthropic:*"));
        assert!(name_matches("anything", "*"));
    }

    #[test]
    fn a_name_or_a_prefix_is_a_pattern_and_a_regular_expression_is_not() {
        for of in [PatternOf::Tools, PatternOf::Models] {
            for good in [
                "Bash",
                "mcp__github__*",
                "*",
                "anthropic:claude-opus-5-5",
                "openrouter:anthropic/claude-haiku-5.5",
                "openrouter:google/*",
            ] {
                assert_eq!(check(good, of), Ok(()), "{good}");
            }
            assert_eq!(check("", of), Err(BadPattern::Empty));
            for bad in [
                "Edit|Write",
                "(Bash)",
                "^Bash$",
                "Bash+",
                "Bas?",
                "[BE]ash",
                "mcp\\_x",
                "a*b",
                "*Bash",
                "mcp__a__*b*",
                "**",
            ] {
                assert_eq!(check(bad, of), Err(BadPattern::NotAPattern), "{bad}");
            }
        }
    }

    /// A `.*` end is Claude Code's regular expression on a tool's name, which
    /// never holds a dot; on a model spec it is the prefix up to the dot.
    #[test]
    fn a_dot_star_end_is_a_regular_expression_for_tools_only() {
        for tool in ["Bash.*", "mcp__memory__.*"] {
            assert_eq!(
                check(tool, PatternOf::Tools),
                Err(BadPattern::NotAPattern),
                "{tool}"
            );
        }
        let gpt_6_1 = "openrouter:openai/gpt-6.*";
        for model in [
            gpt_6_1,
            "openrouter:qwen/qwen3.*",
            "openrouter:z-ai/glm-5.*",
        ] {
            assert_eq!(check(model, PatternOf::Models), Ok(()), "{model}");
        }
        assert!(name_matches("openrouter:openai/gpt-6.1-sol", gpt_6_1));
        assert!(!name_matches("openrouter:openai/gpt-6-luna", gpt_6_1));
    }

    /// A `,` or a space is in no name, so a text holding one is a list that
    /// would match nothing: `Edit,Write` is two names.
    #[test]
    fn a_list_is_no_pattern() {
        for of in [PatternOf::Tools, PatternOf::Models] {
            for list in ["Edit,Write", "Edit Write", "Edit, Write", "Bash\t", " Bash"] {
                assert_eq!(check(list, of), Err(BadPattern::List), "{list:?}");
            }
        }
    }
}
