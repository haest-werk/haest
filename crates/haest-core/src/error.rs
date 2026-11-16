//! Showing an error to the human or the model.
//!
//! haest's errors follow one rule: a variant that carries a source error
//! never prints it in its own `Display`; the source is reached through
//! [`Error::source`]. So the text of an error alone is only its top line, and
//! [`report`] is how any of them is shown: the error and every cause under
//! it.

use std::error::Error;

/// The error a trait a host implements returns where haest only shows it (a
/// mod's events): any error (`e.into()`, `?`) or a line of text
/// (`"why".into()`). Show it with [`report`] (`report(&*e)`).
pub type BoxError = Box<dyn Error + Send + Sync>;

/// `e` and every cause under it, joined by `": "`: the line a human or a
/// model reads (an error's own text is often generic, its cause says what
/// broke).
///
/// ```
/// use haest_core::error::report;
///
/// #[derive(Debug, thiserror::Error)]
/// #[error("reading the session")]
/// struct Unread(#[source] std::io::Error);
///
/// let e = Unread(std::io::Error::other("permission denied"));
/// assert_eq!(e.to_string(), "reading the session");
/// assert_eq!(report(&e), "reading the session: permission denied");
/// ```
pub fn report(e: &dyn Error) -> String {
    let mut line = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        line.push_str(": ");
        line.push_str(&c.to_string());
        cause = c.source();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer")]
    struct Outer(#[source] Middle);

    #[derive(Debug, thiserror::Error)]
    #[error("middle")]
    struct Middle(#[source] std::io::Error);

    #[test]
    fn the_report_walks_every_cause_in_order() {
        let e = Outer(Middle(std::io::Error::other("reset by peer")));
        assert_eq!(report(&e), "outer: middle: reset by peer");
        let alone = std::io::Error::other("alone");
        assert_eq!(report(&alone), "alone");
    }
}
