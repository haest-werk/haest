//! `${NAME}` and `${NAME:-default}` in a config value: Claude Code's
//! `.mcp.json` syntax, and the one haest uses wherever a file names a variable.
//!
//! A helper the haest crates share (`haest-config`, `haest-mcp`); the loop
//! does not use it. Start from [`expand_vars`].

/// `text` with its `${NAME}` and `${NAME:-default}` filled in from `lookup`,
/// plus the names that had neither a value nor a default.
///
/// An unset name with no default stays as written, and is named once in the
/// list for the caller to report ([`unset_note`]). A variable set to "" is
/// set, so its default is not used. `$NAME` without braces, and a `${...}`
/// whose inside is not a variable name ([`is_var_name`]), stay as written and
/// are not reported. Nothing is expanded twice: a value that holds `${X}`
/// stays as it came.
///
/// # Examples
///
/// ```
/// use haest_core::__private::{expand_vars, unset_note};
///
/// let env = |name: &str| (name == "TOKEN").then(|| "t0k".to_string());
/// let (text, unset) = expand_vars("${BASE:-https://api.example.com}/mcp?k=${TOKEN}&u=${USER_ID}", &env);
/// assert_eq!(text, "https://api.example.com/mcp?k=t0k&u=${USER_ID}");
/// assert_eq!(unset, ["USER_ID"]);
/// assert_eq!(unset_note(&unset), "names ${USER_ID}, which is not set and has no default");
/// ```
pub fn expand_vars(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut unset: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(len) = rest[start..].find('}') else {
            rest = &rest[start..];
            break;
        };
        let written = &rest[start..=start + len];
        let inner = &written[2..written.len() - 1];
        let (name, default) = match inner.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (inner, None),
        };
        let value = is_var_name(name)
            .then(|| lookup(name).or_else(|| default.map(str::to_string)))
            .flatten();
        match value {
            Some(v) => out.push_str(&v),
            None => {
                out.push_str(written);
                if is_var_name(name) && !unset.iter().any(|n| n == name) {
                    unset.push(name.to_string());
                }
            }
        }
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    (out, unset)
}

/// [`expand_vars`]'s unset names as the end of a sentence: `names ${A}, which
/// is not set and has no default` (`are`/`have` for more than one). The caller
/// writes the start (`env TOKEN names ${A}, ...`).
pub fn unset_note(unset: &[String]) -> String {
    let names: Vec<String> = unset.iter().map(|n| format!("${{{n}}}")).collect();
    let (is, has) = if names.len() == 1 {
        ("is", "has")
    } else {
        ("are", "have")
    };
    format!(
        "names {}, which {is} not set and {has} no default",
        names.join(", ")
    )
}

/// Is `s` a shell variable name: an ASCII letter or `_`, then ASCII letters,
/// digits and `_`?
pub fn is_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(text: &str) -> (String, Vec<String>) {
        let lookup = |name: &str| match name {
            "TOKEN" => Some("t0k".to_string()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        expand_vars(text, &lookup)
    }

    #[test]
    fn a_set_variable_is_its_value_and_an_unset_one_takes_its_default() {
        assert_eq!(expand("Bearer ${TOKEN}").0, "Bearer t0k");
        assert_eq!(
            expand("${BASE:-https://api.example.com}/mcp").0,
            "https://api.example.com/mcp"
        );
        assert_eq!(expand("[${EMPTY:-fallback}]").0, "[]");
        assert_eq!(expand("${TOKEN}${TOKEN}").0, "t0kt0k");
    }

    #[test]
    fn an_unset_variable_with_no_default_stays_as_written_and_is_named_once() {
        let (text, unset) = expand("${MISSING} and ${MISSING} and ${TOKEN}");
        assert_eq!(text, "${MISSING} and ${MISSING} and t0k");
        assert_eq!(unset, ["MISSING"]);
    }

    #[test]
    fn only_braced_names_expand() {
        for text in [
            "$TOKEN",
            "${not a name}",
            "${1X}",
            "${TOKEN",
            "cost: $5 ${}",
        ] {
            assert_eq!(expand(text), (text.to_string(), Vec::new()), "{text}");
        }
    }
}
