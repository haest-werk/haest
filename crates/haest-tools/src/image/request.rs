//! One call to the Image API (`POST /images`): the request body is built and
//! checked by the tool; this sends it and reads the answer
//! (`data[0].b64_json`, `usage.cost`). A refusal comes back verbatim, with the
//! provider's own words when it gave them: the model tunes its next call from
//! it. Ported from the TS haest's `tools/image-request.ts`.
//!
//! PLUG POINT: [`ImageHttp`], the two HTTP exchanges the tool makes; a host
//! supplies its own transport, a test a fake. [`ReqwestImageHttp`] is the default.

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use haest_core::tool::spelled;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// One HTTP response from the Image API: its status and whole body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HttpReply {
    /// The HTTP status code.
    pub status: u16,
    /// The whole body.
    pub body: Vec<u8>,
}

impl HttpReply {
    /// A reply with `body` as its JSON text (a fake API's answer).
    pub fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            body: body.to_string().into_bytes(),
        }
    }

    /// Whether the status is a success (2xx).
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// PLUG POINT: the two HTTP exchanges GenerateImage makes. An Err is a
/// transport failure, said to the model as is; an HTTP error status is a
/// reply, not an Err.
#[async_trait]
pub trait ImageHttp: Send + Sync {
    /// GET `url` (the model list; no key). Gives up promptly when `cancel` fires.
    async fn get(&self, url: &str, cancel: CancellationToken) -> Result<HttpReply, String>;
    /// POST `body` as JSON to `url` with `Authorization: Bearer <api_key>`.
    async fn post_json(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
        cancel: CancellationToken,
    ) -> Result<HttpReply, String>;
}

/// The default [`ImageHttp`]: reqwest with rustls, no proxy (none from the
/// environment either: a proxy would be sent the API key).
#[derive(Debug, Default, Clone, Copy)]
pub struct ReqwestImageHttp;

impl ReqwestImageHttp {
    async fn send(
        req: reqwest::RequestBuilder,
        cancel: CancellationToken,
    ) -> Result<HttpReply, String> {
        let exchange = async {
            let res = req.send().await.map_err(|e| e.to_string())?;
            let status = res.status().as_u16();
            let body = res.bytes().await.map_err(|e| e.to_string())?.to_vec();
            Ok(HttpReply { status, body })
        };
        tokio::select! {
            r = exchange => r,
            _ = cancel.cancelled() => Err("interrupted".into()),
        }
    }

    fn client() -> Result<reqwest::Client, String> {
        // reqwest takes the process-wide provider when one is installed; this
        // installs ring only when nobody has installed one yet.
        let _ = rustls::crypto::ring::default_provider().install_default();
        // A proxy from HTTP(S)_PROXY / ALL_PROXY would be sent the API key (D15).
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| e.to_string())
    }
}

#[async_trait]
impl ImageHttp for ReqwestImageHttp {
    async fn get(&self, url: &str, cancel: CancellationToken) -> Result<HttpReply, String> {
        Self::send(Self::client()?.get(url), cancel).await
    }

    async fn post_json(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
        cancel: CancellationToken,
    ) -> Result<HttpReply, String> {
        let req = Self::client()?
            .post(url)
            .bearer_auth(api_key)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        Self::send(req, cancel).await
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
    http: &dyn ImageHttp,
    base_url: &str,
    api_key: &str,
    model: &str,
    body: &Value,
    cancel: CancellationToken,
) -> Result<ImageReply, String> {
    let url = format!("{base_url}/images");
    let res = http.post_json(&url, api_key, body, cancel).await?;
    let answer: Value = serde_json::from_slice(&res.body).unwrap_or(Value::Null);
    if !res.ok() {
        return Err(refusal(model, res.status, &answer));
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
