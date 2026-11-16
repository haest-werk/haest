//! An image model's settings, checked BEFORE anything is paid for. Each model on
//! the Image API publishes what it takes (`supported_parameters` in
//! `GET /images/models`): `{type: 'enum', values}`, `{type: 'range', min, max}`,
//! or `{type: 'boolean'}` meaning "takes it". Every refusal names the model and
//! lists what it takes, so the model fixes the call and tries again.
//! [`check_image_settings`] is the check; [`describe_model`] the one-line
//! summary. Ported from the TS haest's `tools/image-settings.ts`.

mod values;

use std::collections::HashMap;

use haest_core::tool::spelled;
use serde::Deserialize;
use serde_json::{Map, Value};
use values::{check_one, free_value};

/// What a model publishes for one setting.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct SettingSpec {
    /// `enum`, `range` or `boolean` ("takes it"); None or another word is
    /// taken as "takes it" too.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// An `enum`'s allowed values.
    #[serde(default)]
    pub values: Option<Vec<Value>>,
    /// A `range`'s lowest whole number; None = no bound.
    #[serde(default)]
    pub min: Option<f64>,
    /// A `range`'s highest whole number; None = no bound.
    #[serde(default)]
    pub max: Option<f64>,
}

/// A model's settings by name, as the Image API's model list publishes them.
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
    /// A setting GenerateImage does not have at all.
    UnknownSetting,
    /// A setting this model does not take.
    NotOffered,
    /// A value the model does not take for a setting it has.
    BadValue,
    /// `n`: GenerateImage makes one picture per call.
    OnePerCall,
    /// SVG out, or a model that only makes SVG: it is never written.
    Svg,
    /// A transparent background asked for as a JPEG.
    TransparentJpeg,
}

/// A call's settings refused, with a message for the model that names what
/// the model takes.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ImageSettingsError {
    /// Which rule refused it.
    pub code: ImageSettingsCode,
    /// What to tell the model.
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
///
/// A null setting counts as not given. With no list to check against, only
/// the names, `seed`, `size` and the rules no model states (no SVG, no
/// transparent JPEG) are checked.
///
/// # Errors
///
/// The first setting refused, with its [`ImageSettingsCode`].
///
/// # Examples
///
/// ```
/// use haest_tools::image::settings::{ImageSettingsCode, ModelSettings, SettingSpec, check_image_settings};
/// use serde_json::{Map, json};
///
/// let aspect = SettingSpec {
///     kind: Some("enum".into()),
///     values: Some(vec![json!("1:1"), json!("16:9")]),
///     ..Default::default()
/// };
/// let model = ModelSettings::from([("aspect_ratio".to_string(), aspect)]);
/// let given = |v: serde_json::Value| v.as_object().cloned().unwrap_or_else(Map::new);
///
/// let fields = check_image_settings("vendor/pic", Some(&model), &given(json!({}))).unwrap();
/// assert_eq!(fields["aspect_ratio"], "1:1");
/// let err = check_image_settings("vendor/pic", Some(&model), &given(json!({"quality": "high"})));
/// assert_eq!(err.unwrap_err().code, ImageSettingsCode::NotOffered);
/// ```
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
