//! Cutting long tool output down to its head and tail.

/// Keep the head and tail of long output; the middle is where nothing useful
/// lives. `max` counts characters, never splitting one.
///
/// Returns the text and whether it was cut. A cut text keeps `max / 2`
/// characters at each end around a line saying how many were dropped, so it
/// is a little longer than `max`.
///
/// ```
/// use haest_core::tool::truncate_middle;
///
/// let (short, cut) = truncate_middle("abc", 10);
/// assert_eq!((short.as_str(), cut), ("abc", false));
///
/// let (long, cut) = truncate_middle(&"x".repeat(100), 10);
/// assert!(cut && long.starts_with("xxxxx") && long.ends_with("xxxxx"));
/// ```
pub fn truncate_middle(text: &str, max: usize) -> (String, bool) {
    let len = text.chars().count();
    if len <= max {
        return (text.to_string(), false);
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(len - half).collect();
    (
        format!(
            "{head}\n\n... [{} characters truncated] ...\n\n{tail}",
            len - max
        ),
        true,
    )
}

/// The default `truncate_middle` budget, in characters.
pub const TRUNCATE_CHARS: usize = 30_000;
