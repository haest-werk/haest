//! `${NAME}` and `${NAME:-default}` in a config value: Claude Code's
//! `.mcp.json` syntax, and the one kern uses wherever a file names a variable.

/// `text` with its variables filled in from `lookup`, and the names that had
/// neither a value nor a default: those stay as written, for the caller to
/// report. A variable set to "" is set, so its default is not used. `$NAME`
/// without braces, and `${...}` that is not a variable name, stay as written.
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

/// [`expand_vars`]'s unset names, as a note says them: `names ${A}, which is
/// not set and has no default`.
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

/// A shell variable name: a letter or `_`, then letters, digits and `_`.
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
