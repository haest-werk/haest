//! Ported from tools/image.test.ts. The Image API is a fake transport; the
//! workspace is in memory. Runs through `run_call`, as the loop does, so a
//! refusal is an error result.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    __private::{ToolIndex, run_call},
    fs::Fs,
    messages::{MediaKind, ToolCall},
    tool::{Tool, ToolContext, ToolOutput, ToolServices},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{HttpReply, ImageHttp, ImageTool, ImageToolOptions};
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

fn image_answer(bytes: &[u8]) -> HttpReply {
    use base64::{Engine, engine::general_purpose::STANDARD};
    HttpReply::json(
        200,
        &json!({"data": [{"b64_json": STANDARD.encode(bytes)}], "usage": {"cost": 0.04}}),
    )
}

struct FakeApi {
    list: Mutex<HttpReply>,
    answer: Mutex<HttpReply>,
    gets: Mutex<usize>,
    /// (url, bearer key, body) of every POST.
    posted: Mutex<Vec<(String, String, Value)>>,
}

#[async_trait]
impl ImageHttp for FakeApi {
    async fn get(&self, url: &str, _: CancellationToken) -> Result<HttpReply, String> {
        assert!(url.ends_with("/images/models"));
        *self.gets.lock().unwrap() += 1;
        Ok(self.list.lock().unwrap().clone())
    }
    async fn post_json(
        &self,
        url: &str,
        key: &str,
        body: &Value,
        _: CancellationToken,
    ) -> Result<HttpReply, String> {
        self.posted
            .lock()
            .unwrap()
            .push((url.into(), key.into(), body.clone()));
        Ok(self.answer.lock().unwrap().clone())
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
    tool: Arc<dyn Tool>,
}

impl Rig {
    fn new() -> Self {
        let api = Arc::new(FakeApi {
            list: Mutex::new(HttpReply::json(200, &models())),
            answer: Mutex::new(image_answer(&JPEG)),
            gets: Mutex::default(),
            posted: Mutex::default(),
        });
        let mut opts = ImageToolOptions::new("k").with_models("img-model", ["text-model"]);
        opts.base_url = BASE.into();
        opts.http = api.clone();
        Self {
            api,
            ws: TestWorkspace::with_files([("/p/ref.png", [1u8, 2])]),
            published: Arc::default(),
            tool: Arc::new(ImageTool::new(opts)),
        }
    }

    async fn run(&self, input: Value) -> ToolOutput {
        let mut ctx = ToolContext::new(CWD, self.ws.clone());
        ctx.services = self.published.clone();
        let call = ToolCall {
            id: "g".into(),
            name: "GenerateImage".into(),
            input: testkit::input(input),
        };
        run_call(&ToolIndex::new(vec![self.tool.clone()]), &call, &ctx).await
    }

    fn posted(&self) -> Vec<(String, String, Value)> {
        self.api.posted.lock().unwrap().clone()
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
    *r.api.list.lock().unwrap() = HttpReply {
        status: 503,
        body: b"down".to_vec(),
    };
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
    *r.api.answer.lock().unwrap() = HttpReply::json(400, &json!({"error": {"message": "unsafe"}}));
    assert!(r.run(json!({"prompt": "x"})).await.is_error);
    *r.api.answer.lock().unwrap() = HttpReply::json(200, &json!({"data": []}));
    assert!(r.run(json!({"prompt": "x"})).await.is_error);
    // An SVG (a page a browser would run) is never written.
    let svg = HttpReply::json(
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

#[tokio::test]
async fn the_model_list_is_read_once_and_kept() {
    let r = Rig::new();
    r.run(json!({"prompt": "x"})).await;
    r.run(json!({"prompt": "y"})).await;
    assert_eq!(*r.api.gets.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_gif_is_saved_as_a_gif() {
    let r = Rig::new();
    *r.api.answer.lock().unwrap() = image_answer(b"GIF89a....");
    let out = r.run(json!({"prompt": "x", "file_path": "anim.png"})).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(r.ws.mem.exists(std::path::Path::new("/p/anim.gif")).await);
}
