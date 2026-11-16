//! One call to the Image API (`POST /images`): the request body is built and
//! checked by the tool; this sends it and reads the answer
//! (`data[0].b64_json`, `usage.cost`). A refusal comes back verbatim, with the
//! provider's own words when it gave them: the model tunes its next call from
//! it. Ported from the TS haest's `tools/image-request.ts`.
//!
//! The tool's two exchanges (`GET /images/models`, `POST /images`) go through
//! the [`HttpClient`] in [`ImageToolOptions::http`](super::ImageToolOptions::http).

use base64::{Engine, engine::general_purpose::STANDARD};
use haest_core::tool::spelled;
use haest_http::{Bytes, HttpClient, Request};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// The largest answer read: far past any image an Image API returns, short of
/// a runaway body filling memory.
const MAX_ANSWER: usize = 64 * 1024 * 1024;

/// Send `req` and read the whole answer: its status and body. An HTTP error
/// status is an answer, not an Err.
///
/// # Errors
///
/// No answer (said to the model as is), or `cancel` fired.
pub(super) async fn exchange(
    http: &dyn HttpClient,
    req: Request,
    cancel: CancellationToken,
) -> Result<(u16, Bytes), String> {
    let exchange = async {
        let res = http.send(req).await.map_err(|e| e.report())?;
        let body = res.body.bytes(MAX_ANSWER).await.map_err(|e| e.report())?;
        Ok((res.status, body))
    };
    tokio::select! {
        r = exchange => r,
        _ = cancel.cancelled() => Err("interrupted".into()),
    }
}

/// The picture an image model made.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageReply {
    /// The image, decoded.
    pub bytes: Vec<u8>,
    /// What the bytes are (`image/png`, `image/jpeg`, `image/webp`,
    /// `image/gif`): read from the bytes, else as the answer named it.
    pub media_type: String,
    /// The model that made it.
    pub model: String,
    /// What the API billed for it, when it said.
    pub cost_usd: Option<f64>,
}

/// Bytes expected at an offset.
type Signature = &'static [(usize, &'static [u8])];

/// Each format's signature (WebP is `RIFF....WEBP`).
const SIGNATURES: [(&str, Signature); 4] = [
    ("image/png", &[(0, &[0x89, 0x50, 0x4e, 0x47])]),
    ("image/jpeg", &[(0, &[0xff, 0xd8, 0xff])]),
    ("image/webp", &[(0, b"RIFF"), (8, b"WEBP")]),
    ("image/gif", &[(0, b"GIF")]),
];

/// What the bytes are, by their first bytes (a model may say png and send
/// jpeg): `image/png`, `image/jpeg`, `image/webp` or `image/gif`, else None.
///
/// ```
/// use haest_tools::image::sniff_image_type;
///
/// assert_eq!(sniff_image_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
/// assert_eq!(sniff_image_type(b"<svg"), None);
/// ```
pub fn sniff_image_type(b: &[u8]) -> Option<&'static str> {
    let at = |offset: usize, sig: &[u8]| b.get(offset..offset + sig.len()) == Some(sig);
    SIGNATURES
        .iter()
        .find(|(_, parts)| parts.iter().all(|(o, sig)| at(*o, sig)))
        .map(|(t, _)| *t)
}

fn refusal(model: &str, status: u16, answer: &Value) -> String {
    let error = &answer["error"];
    let why = error["message"].as_str().unwrap_or("no reason given");
    let raw = match &error["metadata"]["raw"] {
        Value::Null => String::new(),
        other => spelled(other),
    };
    let provider = if !raw.is_empty() && raw != why {
        let raw: String = raw.chars().take(600).collect();
        format!(" (provider: {raw})")
    } else {
        String::new()
    };
    format!("{model} refused the request (HTTP {status}): {why}{provider}")
}

/// Send one image request (`POST {base_url}/images` with `body`) and read the
/// picture out of the answer (`data[0].b64_json`, `usage.cost`).
///
/// `body` is sent as given: GenerateImage checks it first.
///
/// # Errors
///
/// The reason, said to the model as is: the transport failed, the API refused
/// (its status and its own words, the provider's too when it gave them), the
/// answer has no image, or the image is not a PNG, JPEG, WebP or GIF (an SVG
/// never is).
pub async fn call_image_api(
    http: &dyn HttpClient,
    base_url: &str,
    api_key: &str,
    model: &str,
    body: &Value,
    cancel: CancellationToken,
) -> Result<ImageReply, String> {
    let req = Request::post(format!("{base_url}/images"))
        .with_header("authorization", format!("Bearer {api_key}"))
        .with_json(body);
    let (status, res) = exchange(http, req, cancel).await?;
    let answer: Value = serde_json::from_slice(&res).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(refusal(model, status, &answer));
    }
    let first = &answer["data"][0];
    let Some(b64) = first["b64_json"].as_str().filter(|s| !s.is_empty()) else {
        let why = answer["error"]["message"]
            .as_str()
            .unwrap_or("no image in the answer");
        return Err(format!("{model} made no image: {why}"));
    };
    let not_an_image = || {
        format!(
            "{model} answered with something that is not a png/jpeg/webp/gif image; nothing was written"
        )
    };
    let bytes = STANDARD.decode(b64.trim()).map_err(|_| not_an_image())?;
    let media_type = sniff_image_type(&bytes)
        .map(String::from)
        .or_else(|| first["media_type"].as_str().map(String::from))
        .filter(|t| t != "image/svg+xml")
        .ok_or_else(not_an_image)?;
    Ok(ImageReply {
        bytes,
        media_type,
        model: model.into(),
        cost_usd: answer["usage"]["cost"].as_f64(),
    })
}
