//! What a caught panic said. `panic!("literal")` carries a `&'static str`,
//! a formatted `panic!` or an `expect` a `String`; any other payload
//! (`std::panic::panic_any`) says nothing haest can show.
//!
//! A helper the haest crates share (a tool or a script engine that catches a
//! panic and reports it instead of dying); hosts rarely need it.

use std::any::Any;

/// The panic's text, None when its payload is not text. Pass the payload
/// itself, `payload.as_ref()` on the `Box` that `catch_unwind` returns: a
/// `&Box` coerces to the box as the `Any` and never matches.
///
/// # Examples
///
/// ```
/// use haest_core::panics::panic_message;
///
/// fn tool() {
///     panic!("disk on fire");
/// }
///
/// let payload = std::panic::catch_unwind(tool).unwrap_err();
/// assert_eq!(panic_message(payload.as_ref()), Some("disk on fire"));
/// ```
pub fn panic_message(payload: &(dyn Any + Send)) -> Option<&str> {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return Some(s);
    }
    payload.downcast_ref::<String>().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use std::panic::{UnwindSafe, catch_unwind, panic_any};

    use super::*;

    fn caught(f: impl FnOnce() + UnwindSafe) -> Box<dyn Any + Send> {
        catch_unwind(f).expect_err("the closure panics")
    }

    #[test]
    fn a_panic_says_its_text_literal_or_formatted_and_nothing_otherwise() {
        let literal = caught(|| panic!("boom"));
        assert_eq!(panic_message(literal.as_ref()), Some("boom"));
        let n = 7;
        let formatted = caught(move || panic!("boom {n}"));
        assert_eq!(panic_message(formatted.as_ref()), Some("boom 7"));
        let opaque = caught(|| panic_any(7_u8));
        assert_eq!(panic_message(opaque.as_ref()), None);
    }
}
