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

/// Why a text is not a pattern ([`check`]): read by [`name_matches`], it
/// would match no name, so a setting holding it would quietly do nothing.
/// Each setting says it in its own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BadPattern {
    /// The empty text.
    #[error("an empty pattern matches nothing")]
    Empty,
    /// A regular expression (`|`, `(`, `[`, `{`, `^`, `$`, `+`, `?`, `\`, a
    /// `.*` end), or a `*` that is not the last character: haest reads no
    /// regular expression, and `*` only as the end of a prefix.
    #[error(
        "not haest's pattern: an exact name, or a prefix ending in `*`; a regular expression is not read, and `*` only ends a prefix"
    )]
    NotAPattern,
}

/// Whether `pattern` is haest's pattern: not empty, at most one `*` and only
/// at its end, nothing a regular expression would mean. Claude Code's
/// matchers are regular expressions (`Edit|Write`, `Bash.*`), and a file
/// copied from it must fail to load rather than match nothing.
///
/// A prefix that ends in a dot before its `*` (`gpt-6.*`) reads as a
/// regular expression too, and is refused: write it without the dot.
///
/// # Errors
///
/// [`BadPattern`] when it is not one: empty, or not a name or a prefix.
pub fn check(pattern: &str) -> Result<(), BadPattern> {
    const REGEX: &[char] = &['|', '(', ')', '[', ']', '{', '}', '^', '$', '+', '?', '\\'];
    if pattern.is_empty() {
        return Err(BadPattern::Empty);
    }
    let star_inside = (pattern.find('*')).is_some_and(|at| at + 1 != pattern.len());
    if pattern.contains(REGEX) || pattern.ends_with(".*") || star_inside {
        return Err(BadPattern::NotAPattern);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BadPattern, check, name_matches};

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
        for good in [
            "Bash",
            "mcp__github__*",
            "*",
            "anthropic:claude-opus-5-5",
            "openrouter:anthropic/claude-haiku-5.5",
            "openrouter:google/*",
        ] {
            assert_eq!(check(good), Ok(()), "{good}");
        }
        assert_eq!(check(""), Err(BadPattern::Empty));
        for bad in [
            "Edit|Write",
            "(Bash)",
            "Bash.*",
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
            assert_eq!(check(bad), Err(BadPattern::NotAPattern), "{bad}");
        }
    }
}
