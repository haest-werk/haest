//! A name made from free text: an agent's address, a generated file's name, a
//! board card's id.
//!
//! A helper the haest crates share (`haest-tools`, `haest-providers`,
//! `haest-issues`), so every generated name is spelled one way.

/// `Find the bug!` -> `find-the-bug`: lowercase ASCII letters and digits, every
/// other run one dash, none at the ends, at most `max` characters; `fallback`
/// when nothing is left.
///
/// A letter outside ASCII counts as "other" (`café` -> `caf`). A cut at
/// `max` that ends on a dash drops it (`slug("ab cd", 3, ..)` is `ab`).
///
/// # Examples
///
/// ```
/// use haest_core::__private::slug;
///
/// assert_eq!(slug("  Find the BUG!  in x.ts", 24, "agent"), "find-the-bug-in-x-ts");
/// assert_eq!(slug("!!!", 24, "agent"), "agent");
/// ```
pub fn slug(text: &str, max: usize, fallback: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let cut: String = out.chars().take(max).collect();
    let out = cut.trim_end_matches('-');
    if out.is_empty() {
        fallback.into()
    } else {
        out.into()
    }
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

    #[test]
    fn the_cut_at_max_leaves_no_dash_at_the_end() {
        assert_eq!(slug("ab cd", 3, "agent"), "ab");
        assert_eq!(slug("a b", 2, "agent"), "a");
    }
}
