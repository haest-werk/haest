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
pub(crate) struct SettingSpec {
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
pub(crate) type ModelSettings = HashMap<String, SettingSpec>;

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

/// Why a call's settings were refused. Its text is for the model: it names the
/// model and, where it helps, what the model takes.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(super) enum ImageSettingsError {
    /// A setting GenerateImage does not have at all.
    #[error("GenerateImage has no setting \"{name}\". {model} takes: {takes}.")]
    UnknownSetting {
        /// The image model.
        model: String,
        /// The setting the call passed.
        name: String,
        /// What the model takes ([`describe_model`]).
        takes: String,
    },
    /// A setting this model does not take.
    #[error("{model} has no \"{name}\" setting. {model} takes: {takes}.")]
    NotOffered {
        /// The image model.
        model: String,
        /// The setting the call passed.
        name: String,
        /// What the model takes ([`describe_model`]).
        takes: String,
    },
    /// A value the model does not take for a setting it has.
    #[error("{model}'s {name} is {expected} (not {value}).")]
    BadValue {
        /// The image model.
        model: String,
        /// The setting.
        name: String,
        /// The value the call passed (shown as JSON).
        value: Value,
        /// What the setting takes: `one of: 1:1, 16:9`, `a whole number from 0 to 100`.
        expected: String,
    },
    /// `n`: GenerateImage makes one picture per call.
    #[error("GenerateImage makes one picture per call; call it again for another.")]
    OnePerCall,
    /// `output_format` svg: a browser runs SVG as a page, so it is never written.
    #[error("output_format svg is never written (a browser runs SVG as a page); use png or webp.")]
    Svg,
    /// A model that only makes SVG ([`svg_only`]), whatever the call.
    #[error("{model} only makes SVG, which is never written; pick another model.")]
    SvgOnly {
        /// The image model.
        model: String,
    },
    /// A transparent background asked for as a JPEG.
    #[error("A transparent background needs output_format png or webp (jpeg has no transparency).")]
    TransparentJpeg,
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
pub(super) fn max_references(params: Option<&ModelSettings>) -> u64 {
    params
        .and_then(|p| p.get("input_references"))
        .and_then(|s| s.max)
        .map_or(0, |m| m.max(0.0) as u64)
}

/// A model that only makes SVG: a browser runs SVG as a page, so it is never written.
pub(super) fn svg_only(params: Option<&ModelSettings>) -> bool {
    let formats = params
        .and_then(|p| p.get("output_format"))
        .map(values_of)
        .unwrap_or_default();
    !formats.is_empty() && formats.iter().all(|f| f == "svg")
}

/// What a model takes, in one line: success results and every refusal say it.
pub(super) fn describe_model(params: Option<&ModelSettings>) -> String {
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
        return Err(ImageSettingsError::Svg);
    }
    if is("background", "transparent") && is("output_format", "jpeg") {
        return Err(ImageSettingsError::TransparentJpeg);
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
    if name == "n" {
        return Err(ImageSettingsError::OnePerCall);
    }
    let unknown = !IMAGE_SETTINGS.contains(&name);
    let spec = params.and_then(|p| p.get(name));
    if unknown || (params.is_some() && spec.is_none()) {
        let (model, name, takes) = (model.into(), name.into(), describe_model(params));
        return Err(if unknown {
            ImageSettingsError::UnknownSetting { model, name, takes }
        } else {
            ImageSettingsError::NotOffered { model, name, takes }
        });
    }
    match spec {
        Some(spec) => check_one(model, name, spec, value),
        None => free_value(model, name, value),
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
/// The first setting refused, as an [`ImageSettingsError`].
pub(super) fn check_image_settings(
    model: &str,
    params: Option<&ModelSettings>,
    given: &Map<String, Value>,
) -> Result<Map<String, Value>, ImageSettingsError> {
    if svg_only(params) {
        let model = model.into();
        return Err(ImageSettingsError::SvgOnly { model });
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
