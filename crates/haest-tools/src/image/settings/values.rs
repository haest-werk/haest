//! One setting's value against the model's spec: an enum's values, a range's
//! whole numbers, and the shape of `seed` and `size` when no spec says more.

use haest_core::tool::spelled;
use serde_json::Value;

use super::{ImageSettingsCode, ImageSettingsError, SettingSpec, refuse, values_of};

/// A whole number, as JSON carries one.
fn whole(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0)
}

/// A `{type: 'boolean'}` (or unknown) setting's value, as the API expects it.
/// A refusal shows the value as JSON (`Value`'s Display).
pub(super) fn free_value(
    model: &str,
    name: &str,
    value: &Value,
) -> Result<Value, ImageSettingsError> {
    use ImageSettingsCode::BadValue;
    if name == "seed" && !whole(value).is_some_and(|n| n >= 0.0) {
        let why = format!("{model}'s seed is a whole number 0 or above (not {value}).");
        return refuse(BadValue, why);
    }
    let is_size = |s: &str| {
        s.split_once('x').is_some_and(|(w, h)| {
            [w, h]
                .iter()
                .all(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
    };
    if name == "size" && !value.as_str().is_some_and(is_size) {
        let why =
            format!("{model}'s size is WIDTHxHEIGHT in pixels, e.g. \"1024x1536\" (not {value}).");
        return refuse(BadValue, why);
    }
    Ok(value.clone())
}

/// Number.MAX_SAFE_INTEGER: a range with no bound on one side.
const SAFE: f64 = 9_007_199_254_740_991.0;

/// `value` for setting `name`, checked against the model's `spec`.
pub(super) fn check_one(
    model: &str,
    name: &str,
    spec: &SettingSpec,
    value: &Value,
) -> Result<Value, ImageSettingsError> {
    use ImageSettingsCode::BadValue;
    match spec.kind.as_deref() {
        Some("enum") => {
            let allowed = values_of(spec);
            let v = spelled(value);
            if !allowed.contains(&v) {
                let why = format!(
                    "{model}'s {name} is one of: {} (not {value}).",
                    allowed.join(", ")
                );
                return refuse(BadValue, why);
            }
            Ok(Value::String(v))
        }
        Some("range") => {
            let (min, max) = (spec.min.unwrap_or(-SAFE), spec.max.unwrap_or(SAFE));
            if !whole(value).is_some_and(|n| n >= min && n <= max) {
                let why = format!(
                    "{model}'s {name} is a whole number from {min} to {max} (not {value})."
                );
                return refuse(BadValue, why);
            }
            Ok(value.clone())
        }
        _ => free_value(model, name, value),
    }
}
