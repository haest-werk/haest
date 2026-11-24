//! Reading a call's input. [`pick`]: the canonical input shape a host's
//! transcript renders, built from the raw input by renaming keys. A key the
//! call did not send stays absent, never `null`.

use haest_core::tool::JsonMap;

/// `(canonical key, input key)` pairs; input keys the call did not send are left out.
pub(crate) fn pick(input: &JsonMap, keys: &[(&str, &str)]) -> JsonMap {
    keys.iter()
        .filter_map(|(to, from)| input.get(*from).map(|v| ((*to).to_string(), v.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renames_present_keys_and_leaves_absent_ones_out() {
        let input = json!({"file_path": "a.txt", "offset": 2});
        let out = pick(
            input.as_object().unwrap(),
            &[
                ("path", "file_path"),
                ("offset", "offset"),
                ("limit", "limit"),
            ],
        );
        assert_eq!(json!(out), json!({"path": "a.txt", "offset": 2}));
    }
}
