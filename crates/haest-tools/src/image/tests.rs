//! Ported from tools/image.test.ts. The Image API is a fake HTTP client; the
//! workspace is in memory. Runs through `run_call`, as the loop does, so a
//! refusal is an error result.

mod overwrite;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    __private::{ToolIndex, run_call},
    ToolRef, Tools,
    fs::Fs,
    messages::{MediaKind, ToolCall},
    tool::{ToolContext, ToolOutput, ToolServices},
};
use haest_http::{HttpClient, HttpError, Method, Request, Response};
use serde_json::{Value, json};

use super::{GenerateImageOptions, GenerateImageTool};
use crate::testkit::{self, CWD, TestWorkspace};

const JPEG: [u8; 10] = [0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0, 0, 0];
const BASE: &str = "https://images.test/api/v1";

fn models() -> Value {
    json!({"data": [
        {"id": "img-model", "supported_parameters": {
            "aspect_ratio": {"type": "enum", "values": ["1:1", "16:9"]},
            "resolution": {"type": "enum", "values": ["1K", "2K"]},
            "input_references": {"type": "range", "min": 0, "max": 2}
        }},
        {"id": "text-model", "supported_parameters": {"quality": {"type": "enum", "values": ["low", "high"]}}}
    ]})
}

/// A canned answer: its status and body.
type Canned = (u16, String);

fn json_answer(status: u16, body: &Value) -> Canned {
    (status, body.to_string())
}

fn image_answer(bytes: &[u8]) -> Canned {
    use base64::{Engine, engine::general_purpose::STANDARD};
    json_answer(
        200,
        &json!({"data": [{"b64_json": STANDARD.encode(bytes)}], "usage": {"cost": 0.04}}),
    )
}

struct FakeApi {
    list: Mutex<Canned>,
    answer: Mutex<Canned>,
    /// The URL of every GET (the model list).
    gets: Mutex<Vec<String>>,
    /// (url, bearer key, body) of every POST.
    posted: Mutex<Vec<(String, String, Value)>>,
}

#[async_trait]
impl HttpClient for FakeApi {
    async fn send(&self, req: Request) -> Result<Response, HttpError> {
        let (status, body) = if req.method == Method::Get {
            self.gets.lock().unwrap().push(req.url.clone());
            self.list.lock().unwrap().clone()
        } else {
            let auth = req.header("authorization").unwrap_or_default();
            let key = auth.strip_prefix("Bearer ").unwrap_or_default().to_string();
            let sent = (req.url.clone(), key, req.json_body());
            self.posted.lock().unwrap().push(sent);
            self.answer.lock().unwrap().clone()
        };
        Ok(Response::new(status, body))
    }
}

#[derive(Default)]
struct Published(Mutex<Vec<(usize, String)>>);

#[async_trait]
impl ToolServices for Published {
    async fn publish(&self, data: Vec<u8>, media_type: &str) -> Option<String> {
        self.0.lock().unwrap().push((data.len(), media_type.into()));
        Some("/file/abc.jpg".into())
    }
}

struct Rig {
    api: Arc<FakeApi>,
    ws: Arc<TestWorkspace>,
    published: Arc<Published>,
    tool: ToolRef,
}

impl Rig {
    fn new() -> Self {
        Self::at(BASE)
    }

    /// The tool over the Image API at `base`.
    fn at(base: &str) -> Self {
        let api = Arc::new(FakeApi {
            list: Mutex::new(json_answer(200, &models())),
            answer: Mutex::new(image_answer(&JPEG)),
            gets: Mutex::default(),
            posted: Mutex::default(),
        });
        let opts = GenerateImageOptions::new("k")
            .with_models("img-model", ["text-model"])
            .with_base_url(base)
            .with_http(api.clone());
        Self {
            api,
            ws: TestWorkspace::from_files([("/p/ref.png", [1u8, 2])]),
            published: Arc::default(),
            tool: ToolRef::new(GenerateImageTool::new(opts)),
        }
    }

    async fn run(&self, input: Value) -> ToolOutput {
        let ctx = ToolContext::new(CWD, self.ws.clone()).with_services(self.published.clone());
        let call = ToolCall {
            id: "g".into(),
            name: "GenerateImage".into(),
            input: testkit::input(input),
        };
        run_call(
            &ToolIndex::new(Tools::from(vec![self.tool.clone()])),
            &call,
            &ctx,
        )
        .await
    }

    fn posted(&self) -> Vec<(String, String, Value)> {
        self.api.posted.lock().unwrap().clone()
    }

    fn gets(&self) -> Vec<String> {
        self.api.gets.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn posts_with_checked_settings_saves_with_the_extension_the_bytes_have_and_shows_it() {
    let r = Rig::new();
    let out = r
        .run(json!({"prompt": "a blue square", "file_path": "art/square.png", "settings": {"aspect_ratio": "16:9"}}))
        .await;
    assert!(!out.is_error, "{}", out.content);
    let posted = r.posted();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].0, format!("{BASE}/images"));
    assert_eq!(posted[0].1, "k", "the host's key, as the bearer");
    let want = json!({"model": "img-model", "prompt": "a blue square", "aspect_ratio": "16:9", "resolution": "1K"});
    assert_eq!(posted[0].2, want);
    assert_eq!(
        r.ws.mem
            .read(std::path::Path::new("/p/art/square.jpg"))
            .await
            .unwrap(),
        JPEG
    );
    let media: Vec<_> = out
        .media
        .iter()
        .map(|m| (m.kind, m.media_type.as_str()))
        .collect();
    assert_eq!(media, [(MediaKind::Image, "image/jpeg")]);
    let result = json!({"kind": "file", "mediaType": "image/jpeg", "bytes": JPEG.len(), "url": "/file/abc.jpg"});
    assert_eq!(out.result, Some(result));
    assert_eq!(out.cost_usd, Some(0.04));
    assert_eq!(
        r.published.0.lock().unwrap().last(),
        Some(&(JPEG.len(), "image/jpeg".into()))
    );
}

/// The API's `usage.cost` below 0 is no price: the model was told the
/// picture cost "$-1.500", and the call carried a refund.
#[tokio::test]
async fn a_cost_below_zero_is_no_price() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let r = Rig::new();
    let refund = json!({"data": [{"b64_json": STANDARD.encode(JPEG)}], "usage": {"cost": -1.5}});
    *r.api.answer.lock().unwrap() = json_answer(200, &refund);
    let out = r.run(json!({"prompt": "x"})).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.cost_usd, None);
    assert!(!out.content.contains('$'), "{}", out.content);
}

#[tokio::test]
async fn another_allowed_model_is_used_when_picked() {
    let r = Rig::new();
    r.run(json!({"prompt": "x", "model": "text-model", "settings": {"quality": "high"}}))
        .await;
    let body = &r.posted()[0].2;
    assert_eq!(
        (&body["model"], &body["quality"]),
        (&json!("text-model"), &json!("high"))
    );
}

#[tokio::test]
async fn reference_images_go_as_input_references() {
    let r = Rig::new();
    r.run(json!({"prompt": "make it red", "reference_images": ["ref.png"]}))
        .await;
    let refs = r.posted()[0].2["input_references"]
        .as_array()
        .cloned()
        .unwrap();
    let types: Vec<_> = refs.iter().map(|p| p["type"].clone()).collect();
    assert_eq!(types, [json!("image_url")]);
    assert!(
        refs[0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
}

#[tokio::test]
async fn a_call_the_model_cannot_take_is_refused_before_any_spend() {
    let r = Rig::new();
    let calls = [
        json!({"prompt": "x", "settings": {"quality": "high"}}),
        json!({"prompt": "x", "settings": {"aspect_ratio": "7:3"}}),
        json!({"prompt": "x", "model": "not-allowed"}),
        json!({"prompt": "x", "model": "text-model", "reference_images": ["ref.png"]}),
        json!({"prompt": "x", "settings": "big"}),
        json!({"prompt": "x", "reference_images": ["missing.png"]}),
    ];
    for c in calls {
        assert!(r.run(c.clone()).await.is_error, "{c}");
    }
    assert!(r.posted().is_empty());
}

#[tokio::test]
async fn an_unreadable_model_list_does_not_block_the_call() {
    let r = Rig::new();
    *r.api.list.lock().unwrap() = (503, "down".into());
    let out = r
        .run(json!({"prompt": "x", "settings": {"quality": "high"}}))
        .await;
    assert!(!out.is_error, "{}", out.content);
    let body = &r.posted()[0].2;
    assert_eq!(
        (&body["quality"], &body["aspect_ratio"]),
        (&json!("high"), &json!("1:1"))
    );
}

#[tokio::test]
async fn a_refusal_or_an_answer_without_an_image_is_an_error_the_model_can_act_on() {
    let r = Rig::new();
    *r.api.answer.lock().unwrap() = json_answer(400, &json!({"error": {"message": "unsafe"}}));
    assert!(r.run(json!({"prompt": "x"})).await.is_error);
    *r.api.answer.lock().unwrap() = json_answer(200, &json!({"data": []}));
    assert!(r.run(json!({"prompt": "x"})).await.is_error);
    // An SVG (a page a browser would run) is never written.
    let svg = json_answer(
        200,
        &json!({"data": [{"b64_json": "PHN2Zz4=", "media_type": "image/svg+xml"}]}),
    );
    *r.api.answer.lock().unwrap() = svg;
    assert!(
        r.run(json!({"prompt": "x", "file_path": "a.svg"}))
            .await
            .is_error
    );
    assert!(!r.ws.mem.exists(std::path::Path::new("/p/a.svg")).await);
}

/// A base URL may carry `user:password@` (a gateway that takes its
/// credentials in the URL): neither the options' Debug output nor the error
/// for a model the Image API does not list shows it.
#[tokio::test]
async fn the_base_urls_credentials_never_show() {
    let base = "https://user:s3cret@images.test/api/v1";
    let opts = GenerateImageOptions::new("k").with_base_url(format!("{base}?key=k3y"));
    let shown = format!("{opts:?}");
    assert!(
        !shown.contains("s3cret") && !shown.contains("k3y"),
        "{shown}"
    );
    assert!(shown.contains("images.test/api/v1"), "{shown}");

    let r = Rig::at(base);
    let unlisted = json!({"data": [{"id": "other", "supported_parameters": {}}]});
    *r.api.list.lock().unwrap() = json_answer(200, &unlisted);
    let out = r.run(json!({"prompt": "x"})).await;
    assert!(out.is_error, "{}", out.content);
    assert!(!out.content.contains("s3cret"), "{}", out.content);
    assert!(
        out.content.contains("images.test/api/v1/images/models"),
        "{}",
        out.content
    );
    assert!(r.posted().is_empty());
}

/// A base URL whose password holds an unescaped `/` reads to a URL parser
/// as another host (`user`, port 1234), which would be sent the key and the
/// rest of the password: the call fails before any request, saying why and
/// how to write the URL.
#[tokio::test]
async fn a_base_url_a_parser_misreads_is_never_sent_to() {
    let r = Rig::at("https://user:1234/s3cret@images.test/api/v1");
    let out = r.run(json!({"prompt": "x"})).await;
    assert!(out.is_error, "{}", out.content);
    assert!(
        !out.content.contains("s3cret") && !out.content.contains("1234"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains("%2F") && out.content.contains("images.test/api/v1"),
        "{}",
        out.content
    );
    assert_eq!(r.gets(), [""; 0], "the model list was asked for");
    assert!(r.posted().is_empty());
}

#[tokio::test]
async fn the_model_list_is_read_once_and_kept() {
    let r = Rig::new();
    r.run(json!({"prompt": "x"})).await;
    r.run(json!({"prompt": "y"})).await;
    assert_eq!(r.gets(), [format!("{BASE}/images/models")]);
}

/// A base URL's query (a gateway's key) stays the query: `/images` and
/// `/images/models` go after the base's path, before it, however many
/// slashes that path ends in. They went after the query, so the API saw
/// the base path, and a key with the path glued onto it.
#[tokio::test]
async fn a_base_urls_query_stays_after_the_path() {
    for base in [
        "https://images.test/api/v1?key=k3y",
        "https://images.test/api/v1/?key=k3y",
    ] {
        let r = Rig::at(base);
        let out = r.run(json!({"prompt": "x"})).await;
        assert!(!out.is_error, "{}", out.content);
        let list = "https://images.test/api/v1/images/models?key=k3y";
        assert_eq!(r.gets(), [list], "{base}");
        let posted: Vec<String> = r.posted().into_iter().map(|p| p.0).collect();
        assert_eq!(
            posted,
            ["https://images.test/api/v1/images?key=k3y"],
            "{base}"
        );
    }
}

/// The list's URL in the error for a model it does not name is the one the
/// list was read from, its query after the path and its values hidden.
#[tokio::test]
async fn the_unknown_model_error_names_the_list_as_it_was_read() {
    let r = Rig::at("https://images.test/api/v1?key=k3y");
    let unlisted = json!({"data": [{"id": "other", "supported_parameters": {}}]});
    *r.api.list.lock().unwrap() = json_answer(200, &unlisted);
    let out = r.run(json!({"prompt": "x"})).await;
    assert!(out.is_error, "{}", out.content);
    assert!(
        out.content
            .contains("(GET https://images.test/api/v1/images/models?key=<redacted>)"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn a_gif_is_saved_as_a_gif() {
    let r = Rig::new();
    *r.api.answer.lock().unwrap() = image_answer(b"GIF89a....");
    let out = r.run(json!({"prompt": "x", "file_path": "anim.png"})).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(r.ws.mem.exists(std::path::Path::new("/p/anim.gif")).await);
}

#[test]
fn an_image_is_known_by_its_first_bytes_and_an_svg_is_none() {
    use super::request::sniff_image_type;
    assert_eq!(sniff_image_type(&JPEG), Some("image/jpeg"));
    assert_eq!(
        sniff_image_type(b"RIFF\0\0\0\0WEBPVP8 "),
        Some("image/webp")
    );
    assert_eq!(sniff_image_type(b"<svg"), None);
}
