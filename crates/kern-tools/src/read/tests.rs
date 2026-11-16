//! Ported from tools/tools.test.ts (Read), plus the intercept plug point.

use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
    messages::{MediaKind, MediaPart},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolHost, ToolOutput},
};
use serde_json::{Value, json};

use super::{MAX_FILE_BYTES, READ_TOKEN_BUDGET, ReadIntercept, ReadTool};
use crate::testkit::{ClaimedSizeFs, TestWorkspace, ctx, input};

async fn read(ws: &Arc<TestWorkspace>, v: Value) -> Result<ToolOutput, ToolError> {
    ReadTool::default()
        .execute(input(v), &ctx(ws.clone()))
        .await
}

fn is_input_error(r: &Result<ToolOutput, ToolError>) -> bool {
    matches!(r, Err(ToolError::Input(_)))
}

/// 3,000 lines of ~100 chars: ~75K tokens, three times Read's default budget.
fn big_file() -> Arc<TestWorkspace> {
    let text: Vec<String> = (0..3000).map(|i| format!("{i:.<96}")).collect();
    TestWorkspace::with_files([("/p/big.txt", text.join("\n"))])
}

fn shown_lines(content: &str) -> usize {
    content
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.split_once('\t')
                .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .count()
}

#[tokio::test]
async fn numbers_lines_and_pages_with_offset_and_limit() {
    let ws = TestWorkspace::with_files([("/p/f.txt", "a\nb\nc\nd")]);
    let r = read(&ws, json!({"file_path": "f.txt", "offset": 2, "limit": 2}))
        .await
        .unwrap();
    let first: Vec<_> = r.content.lines().take(2).collect();
    assert_eq!(first, ["2\tb", "3\tc"]);
    assert!(r.content.contains("offset=4"));
    assert!(
        r.sized,
        "Read fits its own budget; the generic spill must not touch it"
    );
}

#[tokio::test]
async fn refuses_binaries_and_missing_files_as_correctable_errors() {
    let ws = TestWorkspace::with_files([("/p/b.bin", &[1u8, 0, 2][..])]);
    assert!(is_input_error(
        &read(&ws, json!({"file_path": "b.bin"})).await
    ));
    assert!(is_input_error(
        &read(&ws, json!({"file_path": "nope"})).await
    ));
    assert!(is_input_error(&read(&ws, json!({"file_path": "/p"})).await));
}

#[tokio::test]
async fn an_image_comes_back_as_media_the_model_sees_not_as_a_binary_refusal() {
    let png = [0x89u8, 0x50, 0x4e, 0x47, 0, 0, 0, 0];
    let ws = TestWorkspace::with_files([("/p/shot.png", &png[..])]);
    let r = read(&ws, json!({"file_path": "shot.png"})).await.unwrap();
    assert!(!r.is_error);
    assert_eq!(
        r.media,
        [MediaPart {
            kind: MediaKind::Image,
            media_type: "image/png".into(),
            data: "iVBORwAAAAA=".into(),
            name: "shot.png".into(),
        }]
    );
    let res = r.result.unwrap();
    assert_eq!(res["kind"], "file");
    assert_eq!(res["mediaType"], "image/png");
    assert_eq!(res["bytes"], png.len());
    assert!(res.get("url").is_none(), "no host to publish to, no url");
}

struct Publishing;
#[async_trait]
impl ToolHost for Publishing {
    async fn publish(&self, data: Vec<u8>, media_type: &str) -> Option<String> {
        Some(format!("blob://{media_type}/{}", data.len()))
    }
}

#[tokio::test]
async fn a_published_image_carries_its_url_to_the_transcript() {
    let ws = TestWorkspace::with_files([("/p/a.gif", &[1u8, 2, 3][..])]);
    let mut c = ctx(ws);
    c.host = Arc::new(Publishing);
    let r = ReadTool::default()
        .execute(input(json!({"file_path": "a.gif"})), &c)
        .await
        .unwrap();
    assert_eq!(r.result.unwrap()["url"], "blob://image/gif/3");
}

#[tokio::test]
async fn a_read_over_the_token_budget_returns_the_lines_that_fit_and_where_to_continue() {
    let ws = big_file();
    let r = read(&ws, json!({"file_path": "big.txt", "limit": 3000}))
        .await
        .unwrap();
    assert!(!r.is_error);
    assert!(r.content.chars().count() as u64 <= READ_TOKEN_BUDGET * 4 + 200);
    let next = shown_lines(&r.content) + 1;
    assert!(r.content.contains(&format!("offset={next}")));
    assert!(r.content.contains("allow_large"));
}

#[tokio::test]
async fn allow_large_reads_the_whole_file_in_one_call_when_the_context_has_room() {
    let ws = big_file();
    let r = read(&ws, json!({"file_path": "big.txt", "allow_large": true}))
        .await
        .unwrap();
    assert_eq!(shown_lines(&r.content), 3000);
    assert!(!r.content.contains("offset="));
}

#[tokio::test]
async fn allow_large_never_returns_more_than_the_context_has_room_for() {
    let ws = big_file();
    let mut c = ctx(ws);
    c.context_room_tokens = Some(5000);
    let r = ReadTool::default()
        .execute(
            input(json!({"file_path": "big.txt", "allow_large": "true"})),
            &c,
        )
        .await
        .unwrap();
    assert!(r.content.chars().count() <= 5000 * 4 + 200);
    let next = shown_lines(&r.content) + 1;
    assert!(r.content.contains(&format!("offset={next}")));
}

#[tokio::test]
async fn an_image_type_no_provider_accepts_is_still_a_refusal() {
    let ws = TestWorkspace::with_files([("/p/pic.tiff", &[1u8, 0, 2][..])]);
    assert!(is_input_error(
        &read(&ws, json!({"file_path": "pic.tiff"})).await
    ));
}

#[tokio::test]
async fn an_offset_past_the_end_is_an_empty_read_not_an_error() {
    let ws = TestWorkspace::with_files([("/p/f.txt", "a\nb")]);
    let r = read(&ws, json!({"file_path": "f.txt", "offset": 1e300}))
        .await
        .unwrap();
    assert_eq!(shown_lines(&r.content), 0);
    assert!(!r.is_error);
    assert!(is_input_error(
        &read(&ws, json!({"file_path": "f.txt", "offset": "x"})).await
    ));
}

#[tokio::test]
async fn a_closing_newline_ends_the_last_line_and_adds_no_line_of_its_own() {
    // Ten lines, as `wc -l` counts them: the model must see ten, not eleven.
    let ws = TestWorkspace::with_files([("/p/ten.txt", "x\n".repeat(10))]);
    let r = read(&ws, json!({"file_path": "ten.txt"})).await.unwrap();
    assert_eq!(shown_lines(&r.content), 10);
    assert_eq!(r.content.lines().last(), Some("10\tx"));
    // A blank line the file really has is still shown.
    let ws = TestWorkspace::with_files([("/p/gap.txt", "a\n\n")]);
    let r = read(&ws, json!({"file_path": "gap.txt"})).await.unwrap();
    assert_eq!(shown_lines(&r.content), 2);
}

/// Answers `attachment:<id>` itself; everything else goes to the fs.
struct Attachments;
#[async_trait]
impl ReadIntercept for Attachments {
    async fn intercept(
        &self,
        raw_path: &str,
        input: &JsonMap,
        ctx: &ToolContext,
    ) -> Option<Result<ToolOutput, ToolError>> {
        let id = raw_path.strip_prefix("attachment:")?;
        Some(super::text_output(
            &format!("converted {id}\nline two"),
            9,
            input,
            ctx,
            "",
        ))
    }
}

#[tokio::test]
async fn an_intercept_answers_its_own_paths_and_leaves_the_rest_to_the_fs() {
    let ws = TestWorkspace::with_files([("/p/f.txt", "from disk")]);
    let tool = ReadTool::new(Some(Arc::new(Attachments)));
    let c = ctx(ws);
    let att = tool
        .execute(
            input(json!({"file_path": "attachment:42", "offset": 2})),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(att.content, "2\tline two");
    let disk = tool
        .execute(input(json!({"file_path": "f.txt"})), &c)
        .await
        .unwrap();
    assert!(disk.content.contains("from disk"));
    assert!(matches!(
        tool.execute(input(json!({})), &c).await,
        Err(ToolError::Input(_))
    ));
}

#[test]
fn the_canonical_input_names_the_path() {
    let c = ReadTool::default().canonical_input(&input(json!({"file_path": "a", "limit": 5})));
    assert_eq!(json!(c), json!({"path": "a", "limit": 5}));
}

#[tokio::test]
async fn a_file_over_the_size_cap_is_refused_from_its_stat_before_any_of_it_is_read() {
    let huge = ClaimedSizeFs::new([("/p/huge.log", "line\n")], 5 << 30);
    let mut answers = Vec::new();
    for call in [
        json!({"file_path": "huge.log"}),
        json!({"file_path": "huge.log", "offset": 10, "limit": 5}),
    ] {
        answers.push(ReadTool::default().execute(input(call), &huge.ctx()).await);
    }
    assert_eq!(huge.reads(), 0, "Read loaded a 5 GB file whole");
    assert!(answers.iter().all(is_input_error));
    // At the cap a file still reads; one byte over, it does not.
    for (size, reads) in [(MAX_FILE_BYTES, true), (MAX_FILE_BYTES + 1, false)] {
        let fs = ClaimedSizeFs::new([("/p/f.log", "line\n")], size);
        let r = ReadTool::default()
            .execute(input(json!({"file_path": "f.log"})), &fs.ctx())
            .await;
        assert_eq!(r.is_ok(), reads, "{size} bytes");
    }
}
