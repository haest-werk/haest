//! A name made from free text: an agent's address, a generated file's name.

/// `Find the bug!` -> `find-the-bug`: lowercase ASCII letters and digits, every
/// other run one dash, none at the ends, at most `max` characters; `fallback`
/// when nothing is left.
pub(crate) fn slug(text: &str, max: usize, fallback: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(max).collect();
    if out.is_empty() { fallback.into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn free_text_becomes_a_short_dashed_name() {
        assert_eq!(
            slug("  Find the BUG!  in x.ts", 24, "agent"),
            "find-the-bug-in-x-ts"
        );
        assert_eq!(slug("abcdefghij", 4, "agent"), "abcd");
        assert_eq!(slug("!!!", 24, "agent"), "agent");
        assert_eq!(slug("café au lait", 40, "image"), "caf-au-lait");
    }
}
