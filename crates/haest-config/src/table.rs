//! A [`Kind::Table`](crate::Kind::Table) setting's value: TOML in every
//! layer, one inline table in the checked spelling, each layer merged into
//! the ones before it.

use serde::{Serialize, de::DeserializeOwned};
use toml::{Table, Value};

/// `raw` as a table: `key = value` lines (a TOML document, dotted keys and
/// `[sub]` headers too) or one inline table, `{ key = value }`.
pub(crate) fn parse(raw: &str) -> Result<Table, String> {
    let raw = raw.trim();
    if let Ok(table) = raw.parse::<Table>() {
        return Ok(table);
    }
    let inline = format!("v = {raw}").parse::<Table>();
    match inline.map(|mut t| t.remove("v")) {
        Ok(Some(Value::Table(table))) => Ok(table),
        // The value is not echoed: a table may hold a key (a hook's header).
        _ => Err("must be a TOML table: `key = value` lines, or `{ key = value }`".into()),
    }
}

/// `table` in the checked spelling: one inline table, its keys in order.
pub(crate) fn inline(table: Table) -> String {
    Value::Table(table).to_string()
}

/// `raw` checked as a table, in the checked spelling.
pub(crate) fn check(raw: &str) -> Result<String, String> {
    parse(raw).map(inline)
}

/// `later` laid over `into`: a table merges key by key, an array is
/// appended to (an array of tables gains the later layer's entries), any
/// other value is replaced.
pub(crate) fn merge(into: &mut Table, later: Table) {
    for (key, value) in later {
        match (into.get_mut(&key), value) {
            (Some(Value::Table(earlier)), Value::Table(more)) => merge(earlier, more),
            (Some(Value::Array(earlier)), Value::Array(more)) => earlier.extend(more),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

/// `value` as its table; an empty one for a value that is no table.
fn table_of<T: Serialize>(value: &T) -> Table {
    match Value::try_from(value) {
        Ok(Value::Table(table)) => table,
        _ => Table::new(),
    }
}

/// `value` in the checked spelling; None when it writes no key (every key at
/// its default, for a type that writes only what differs).
pub(crate) fn to_raw<T: Serialize>(value: &T) -> Option<String> {
    let table = table_of(value);
    (!table.is_empty()).then(|| inline(table))
}

/// `raw` laid over `current` ([`merge`]) and read back as its type: a key
/// the type does not have, or a value it does not take, is refused, and
/// `current` is left as it was.
pub(crate) fn merged<T: Serialize + DeserializeOwned>(current: &T, raw: &str) -> Result<T, String> {
    let mut table = table_of(current);
    merge(&mut table, parse(raw)?);
    Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| e.message().trim().to_string())
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Hooks {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        on_end: Vec<Hook>,
    }

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Hook {
        command: String,
    }

    #[test]
    fn lines_and_an_inline_table_are_one_spelling() {
        let lines = check("a = 1\nb.c = \"x\"").unwrap();
        let inline = check(r#"{ b = { c = "x" }, a = 1 }"#).unwrap();
        assert_eq!(lines, inline);
        assert_eq!(check(&lines).unwrap(), lines, "the spelling reads back");
        assert_eq!(check("").unwrap(), check("{}").unwrap(), "empty is no key");
    }

    #[test]
    fn what_is_no_table_is_refused_without_echoing_it() {
        for raw in ["1h", "[1, 2]", "a = ", "{ a = 1", "\"s3cret\""] {
            let why = check(raw).unwrap_err();
            assert!(!why.contains("s3cret"), "{why}");
        }
    }

    #[test]
    fn a_later_table_merges_key_by_key_and_an_array_appends() {
        let mut into = parse(
            r#"a = 1
[sub]
x = "keep"
y = "old"
[[list]]
n = 1"#,
        )
        .unwrap();
        let later = parse(r#"{ sub = { y = "new" }, list = [{ n = 2 }], a = 3 }"#).unwrap();
        merge(&mut into, later);
        let want = parse(
            r#"a = 3
sub = { x = "keep", y = "new" }
list = [{ n = 1 }, { n = 2 }]"#,
        )
        .unwrap();
        assert_eq!(into, want);
    }

    #[test]
    fn a_typed_table_takes_each_layer_and_refuses_what_it_lacks() {
        let first = merged(&Hooks::default(), r#"name = "a""#).unwrap();
        let second = merged(&first, r#"on_end = [{ command = "x" }]"#).unwrap();
        let third = merged(&second, r#"on_end = [{ command = "y" }]"#).unwrap();
        assert_eq!(third.name.as_deref(), Some("a"));
        let commands: Vec<_> = third.on_end.iter().map(|h| h.command.as_str()).collect();
        assert_eq!(commands, ["x", "y"]);
        assert!(merged(&third, "nmae = \"b\"").is_err(), "a key it lacks");
        assert!(
            merged(&third, "name = 1").is_err(),
            "a value it does not take"
        );
        assert_eq!(to_raw(&Hooks::default()), None, "every key at its default");
        let raw = to_raw(&first).unwrap();
        assert_eq!(merged(&Hooks::default(), &raw).unwrap(), first);
    }
}
