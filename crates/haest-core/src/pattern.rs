//! The one name pattern haest's settings take: an exact name, or a prefix
//! ending in `*` (`mcp__github__*`, `anthropic:*`).

/// Whether `name` matches `pattern`: `prefix*` matches every name that
/// starts with `prefix` (a lone `*` matches all), anything else only itself.
pub fn name_matches(name: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::name_matches;

    #[test]
    fn an_exact_pattern_matches_only_itself_and_a_star_matches_its_prefix() {
        assert!(name_matches("Bash", "Bash"));
        assert!(!name_matches("BashOutput", "Bash"));
        assert!(name_matches("mcp__github__search", "mcp__github__*"));
        assert!(!name_matches("mcp__gitlab__search", "mcp__github__*"));
        assert!(name_matches("anthropic:claude-opus-5-5", "anthropic:*"));
        assert!(name_matches("anything", "*"));
    }
}
