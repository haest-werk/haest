//! An image model's settings, checked BEFORE anything is paid for. Each model on
//! the Image API publishes what it takes (`supported_parameters` in
//! `GET /images/models`): `{type: 'enum', values}`, `{type: 'range', min, max}`,
//! or `{type: 'boolean'}` meaning "takes it". Every refusal names the model and
//! lists what it takes, so the model fixes the call and tries again.
//! Ported from the TS kern's `tools/image-settings.ts`.

use std::collections::HashMap;

use kern_core::tool::spelled;
use serde::Deserialize;
use serde_json::{Map, Value};

/// What a model publishes for one setting.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct SettingSpec {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub values: Option<Vec<Value>>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

/// A model's settings by name.
pub type ModelSettings = HashMap<String, SettingSpec>;

/// The settings a call may pass, besides what to draw and the references.
const IMAGE_SETTINGS: [&str; 8] = [
    "aspect_ratio",
    "resolution",
    "size",
    "quality",
    "background",
    "output_format",
    "output_compression",
    "seed",
];

/// Why a call was refused: the branch, as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSettingsCode {
    UnknownSetting,
    NotOffered,
    BadValue,
    OnePerCall,
    Svg,
    TransparentJpeg,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ImageSettingsError {
    pub code: ImageSettingsCode,
    pub message: String,
}

fn refuse<T>(code: ImageSettingsCode, message: String) -> Result<T, ImageSettingsError> {
    Err(ImageSettingsError { code, message })
}

fn values_of(spec: &SettingSpec) -> Vec<String> {
    spec.values.iter().flatten().map(spelled).collect()
}

/// One setting as a reader sees it: `quality (auto, low, high)`, `output_compression (0-100)`.
fn describe_setting(name: &str, spec: &SettingSpec) -> String {
    match (spec.kind.as_deref(), spec.min, spec.max) {
        (Some("enum"), _, _) => format!("{name} ({})", values_of(spec).join(", ")),
        (Some("range"), Some(min), Some(max)) => format!("{name} ({min}-{max})"),
        _ if name == "seed" => "seed (a whole number, for repeatable results)".into(),
        _ if name == "size" => "size (WIDTHxHEIGHT, e.g. 1024x1536)".into(),
        _ => name.into(),
    }
}

/// How many reference images a model takes (0 when it says nothing).
pub fn max_references(params: Option<&ModelSettings>) -> u64 {
    params
        .and_then(|p| p.get("input_references"))
        .and_then(|s| s.max)
        .map_or(0, |m| m.max(0.0) as u64)
}

/// A model that only makes SVG: a browser runs SVG as a page, so it is never written.
pub fn svg_only(params: Option<&ModelSettings>) -> bool {
    let formats = params
        .and_then(|p| p.get("output_format"))
        .map(values_of)
        .unwrap_or_default();
    !formats.is_empty() && formats.iter().all(|f| f == "svg")
}

/// What a model takes, in one line: success results and every refusal say it.
pub fn describe_model(params: Option<&ModelSettings>) -> String {
    let Some(params) = params else {
        return "settings unknown (the model list could not be read); pass only what the request needs".into();
    };
    let mut parts: Vec<String> = IMAGE_SETTINGS
        .iter()
        .filter_map(|s| params.get(*s).map(|spec| describe_setting(s, spec)))
        .collect();
    parts.push(match max_references(Some(params)) {
        0 => "no reference images".into(),
        1 => "up to 1 reference image".into(),
        n => format!("up to {n} reference images"),
    });
    parts.join("; ")
}

/// A whole number, as JSON carries one.
fn whole(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0)
}

/// A `{type: 'boolean'}` (or unknown) setting's value, as the API expects it.
/// A refusal shows the value as JSON (`Value`'s Display).
fn free_value(model: &str, name: &str, value: &Value) -> Result<Value, ImageSettingsError> {
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

fn check_one(
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

/// Cross-setting rules no single model spec states.
fn check_combination(out: &Map<String, Value>) -> Result<(), ImageSettingsError> {
    let is = |k: &str, v: &str| out.get(k).and_then(Value::as_str) == Some(v);
    if is("output_format", "svg") {
        let why =
            "output_format svg is never written (a browser runs SVG as a page); use png or webp.";
        return refuse(ImageSettingsCode::Svg, why.into());
    }
    if is("background", "transparent") && is("output_format", "jpeg") {
        let why =
            "A transparent background needs output_format png or webp (jpeg has no transparency).";
        return refuse(ImageSettingsCode::TransparentJpeg, why.into());
    }
    Ok(())
}

/// One setting the call passed: known to GenerateImage, offered by the model, a value it takes.
fn check_given(
    model: &str,
    params: Option<&ModelSettings>,
    name: &str,
    value: &Value,
) -> Result<Value, ImageSettingsError> {
    let takes = || format!("{model} takes: {}.", describe_model(params));
    if name == "n" {
        let why = "GenerateImage makes one picture per call; call it again for another.";
        return refuse(ImageSettingsCode::OnePerCall, why.into());
    }
    if !IMAGE_SETTINGS.contains(&name) {
        let why = format!("GenerateImage has no setting \"{name}\". {}", takes());
        return refuse(ImageSettingsCode::UnknownSetting, why);
    }
    match (params, params.and_then(|p| p.get(name))) {
        (_, Some(spec)) => check_one(model, name, spec, value),
        (Some(_), None) => refuse(
            ImageSettingsCode::NotOffered,
            format!("{model} has no \"{name}\" setting. {}", takes()),
        ),
        (None, None) => free_value(model, name, value),
    }
}

/// Square at 1K, where the model offers them and the call did not say.
fn with_defaults(out: &mut Map<String, Value>, params: Option<&ModelSettings>) {
    let offers = |name: &str, v: &str| {
        params.is_none_or(|p| {
            p.get(name)
                .is_some_and(|s| values_of(s).iter().any(|x| x == v))
        })
    };
    if !out.contains_key("aspect_ratio") && offers("aspect_ratio", "1:1") {
        out.insert("aspect_ratio".into(), "1:1".into());
    }
    let has_resolution = params.is_some_and(|p| p.contains_key("resolution"));
    if !out.contains_key("resolution") && has_resolution && offers("resolution", "1K") {
        out.insert("resolution".into(), "1K".into());
    }
}

/// The request fields for `given` (the call's settings), checked against the
/// model's own (None: the model list could not be read). Defaults where the
/// model offers them: square, at 1K.
pub fn check_image_settings(
    model: &str,
    params: Option<&ModelSettings>,
    given: &Map<String, Value>,
) -> Result<Map<String, Value>, ImageSettingsError> {
    if svg_only(params) {
        let why = format!("{model} only makes SVG, which is never written; pick another model.");
        return refuse(ImageSettingsCode::Svg, why);
    }
    let mut out = Map::new();
    for (name, value) in given.iter().filter(|(_, v)| !v.is_null()) {
        out.insert(name.clone(), check_given(model, params, name, value)?);
    }
    check_combination(&out)?;
    with_defaults(&mut out, params);
    Ok(out)
}

#[cfg(test)]
mod tests;
