//! Ported from tools/image-settings.test.ts. Shapes as the Image API's
//! `/images/models` published them on 2026-11-05.

use serde_json::{Value, json};

use super::{ImageSettingsCode, ModelSettings, check_image_settings, max_references, svg_only};
use crate::testkit::input;
use ImageSettingsCode::*;

fn params(v: Value) -> ModelSettings {
    serde_json::from_value(v).unwrap()
}

fn banana() -> ModelSettings {
    params(json!({
        "resolution": {"type": "enum", "values": ["1K", "2K", "4K"]},
        "aspect_ratio": {"type": "enum", "values": ["1:1", "16:9", "9:16"]},
        "n": {"type": "range", "min": 1, "max": 1},
        "input_references": {"type": "range", "min": 0, "max": 14}
    }))
}

fn gpt() -> ModelSettings {
    params(json!({
        "aspect_ratio": {"type": "enum", "values": ["1:1", "3:2", "auto"]},
        "output_format": {"type": "enum", "values": ["png", "jpeg", "webp"]},
        "quality": {"type": "enum", "values": ["auto", "low", "high"]},
        "background": {"type": "enum", "values": ["auto", "transparent", "opaque"]},
        "output_compression": {"type": "range", "min": 0, "max": 100}
    }))
}

fn check(model: &str, p: Option<&ModelSettings>, v: Value) -> Result<Value, ImageSettingsCode> {
    check_image_settings(model, p, &input(v))
        .map(Value::Object)
        .map_err(|e| e.code)
}

#[test]
fn no_settings_square_at_1k_where_the_model_has_a_resolution_square_only_where_it_does_not() {
    let (b, g) = (banana(), gpt());
    let banana_out = json!({"aspect_ratio": "1:1", "resolution": "1K"});
    assert_eq!(check("banana", Some(&b), json!({})), Ok(banana_out));
    assert_eq!(
        check("gpt", Some(&g), json!({})),
        Ok(json!({"aspect_ratio": "1:1"}))
    );
}

#[test]
fn settings_the_model_takes_pass_through_as_the_api_expects_them() {
    let g = gpt();
    let call = json!({"quality": "high", "output_compression": 80, "background": "transparent"});
    let want = json!({
        "quality": "high",
        "output_compression": 80,
        "background": "transparent",
        "aspect_ratio": "1:1"
    });
    assert_eq!(check("gpt", Some(&g), call), Ok(want));
}

#[test]
fn each_kind_of_bad_call_is_refused_by_branch() {
    let (b, g) = (banana(), gpt());
    assert_eq!(
        check("banana", Some(&b), json!({"quality": "high"})),
        Err(NotOffered)
    );
    assert_eq!(
        check("banana", Some(&b), json!({"colour": "red"})),
        Err(UnknownSetting)
    );
    assert_eq!(
        check("banana", Some(&b), json!({"aspect_ratio": "7:3"})),
        Err(BadValue)
    );
    assert_eq!(
        check("gpt", Some(&g), json!({"output_compression": 101})),
        Err(BadValue)
    );
    assert_eq!(check("banana", Some(&b), json!({"n": 2})), Err(OnePerCall));
    let transparent_jpeg = json!({"background": "transparent", "output_format": "jpeg"});
    assert_eq!(
        check("gpt", Some(&g), transparent_jpeg),
        Err(TransparentJpeg)
    );
}

#[test]
fn with_no_model_list_only_the_shapes_of_seed_and_size_are_checked() {
    let want = json!({"quality": "anything", "seed": 7, "aspect_ratio": "1:1"});
    assert_eq!(
        check("x", None, json!({"quality": "anything", "seed": 7})),
        Ok(want)
    );
    assert_eq!(check("x", None, json!({"seed": -1})), Err(BadValue));
    assert_eq!(check("x", None, json!({"size": "big"})), Err(BadValue));
    assert!(check("x", None, json!({"size": "1024x1536"})).is_ok());
}

#[test]
fn an_svg_only_model_is_refused_whatever_the_call() {
    let svg = params(json!({"output_format": {"type": "enum", "values": ["svg"]}}));
    assert!(svg_only(Some(&svg)));
    assert_eq!(check("svg", Some(&svg), json!({})), Err(Svg));
}

#[test]
fn reference_image_limits_come_from_input_references() {
    let (b, g) = (banana(), gpt());
    let maxes = [
        max_references(Some(&b)),
        max_references(Some(&g)),
        max_references(None),
    ];
    assert_eq!(maxes, [14, 0, 0]);
}
