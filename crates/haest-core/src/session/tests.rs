use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use super::*;
use crate::{
    fs::{DirEntry, Fs, FsStat, MemFs},
    messages::Message,
    provider::Effort,
};

/// A MemFs that remembers every chmod.
#[derive(Default)]
struct ModeFs {
    inner: MemFs,
    modes: Mutex<Vec<(PathBuf, u32)>>,
}

#[async_trait]
impl Fs for ModeFs {
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.inner.read(p).await
    }
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.inner.write(p, d).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.inner.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.inner.stat(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.inner.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir(p).await
    }
    async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        self.inner.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        self.inner.remove_all(p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.rename(a, b).await
    }
    async fn chmod(&self, p: &Path, mode: u32) -> io::Result<()> {
        self.modes.lock().unwrap().push((p.to_path_buf(), mode));
        Ok(())
    }
}

mod interrupted;

const DATA: &str = "/data";

fn store(fs: &Arc<ModeFs>) -> JsonlSessionFile {
    JsonlSessionFile::new(fs.clone(), DATA, "abc").unwrap()
}

fn msg(t: u64, text: &str) -> SessionRecord {
    SessionRecord::Message {
        t,
        message: Message::user(text),
    }
}

fn texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|m| match m {
            Message::User(u) => u.content.clone(),
            _ => String::new(),
        })
        .collect()
}

#[tokio::test]
async fn round_trips_messages_and_config_through_a_private_jsonl_file() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    s.append(&msg(1, "one"));
    s.append(&SessionRecord::Config {
        t: 2,
        model_spec: Some("openrouter:m".into()),
        effort: EffortChange::To(Effort::High),
    });
    s.append(&msg(3, "two line"));
    s.flush().await;
    let back = store(&fs).load().await.unwrap();
    assert_eq!(texts(&back.messages), ["one", "two line"]);
    assert_eq!(back.model_spec.as_deref(), Some("openrouter:m"));
    assert_eq!(back.effort, EffortChange::To(Effort::High));
    let modes = fs.modes.lock().unwrap().clone();
    assert!(modes.contains(&(PathBuf::from("/data/sessions"), 0o700)));
    assert!(modes.contains(&(PathBuf::from("/data/sessions/abc.jsonl"), 0o600)));
}

#[tokio::test]
async fn queued_appends_land_in_call_order_header_first() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    for i in 0..50 {
        s.append(&msg(i, &i.to_string()));
    }
    let back = s.load().await.unwrap();
    let want: Vec<String> = (0..50).map(|i| i.to_string()).collect();
    assert_eq!(texts(&back.messages), want);
    let text = fs.read_to_string(s.path()).await.unwrap();
    assert!(text.starts_with("{\"type\":\"header\""));
}

#[tokio::test]
async fn create_leaves_an_existing_file_alone() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    s.append(&msg(1, "kept"));
    s.create("c2", Path::new("/y"), 5).await.unwrap();
    let back = s.load().await.unwrap();
    assert_eq!(texts(&back.messages), ["kept"]);
    let text = fs.read_to_string(s.path()).await.unwrap();
    assert_eq!(text.matches("\"header\"").count(), 1);
}

#[tokio::test]
async fn compaction_keeps_the_tail_and_prefixes_the_summary() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    for c in ["a", "b", "c", "d"] {
        s.append(&msg(0, c));
    }
    s.append(&SessionRecord::Compaction {
        t: 0,
        summary: "SUM".into(),
        kept: 2,
    });
    s.append(&msg(0, "e"));
    let msgs = texts(&s.load().await.unwrap().messages);
    assert!(msgs[0].contains("SUM"));
    assert_eq!(msgs[1..], ["c", "d", "e"]);
}

#[tokio::test]
async fn a_torn_last_line_is_counted_not_fatal() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", Path::new("/x"), 0).await.unwrap();
    s.append(&msg(0, "ok"));
    s.flush().await;
    let mut torn = b"{\"type\":\"message\",\"t\":0,\"mess".to_vec();
    torn.push(0xE2); // half of a multi-byte character
    fs.append(s.path(), &torn).await.unwrap();
    let back = s.load().await.unwrap();
    assert_eq!(back.messages.len(), 1);
    assert_eq!(back.corrupt_lines, 1);
}

#[test]
fn session_ids_cannot_escape_the_sessions_dir() {
    let dir = sessions_dir(Path::new(DATA));
    assert!(session_file(&dir, "../etc").is_err());
    assert!(session_file(&dir, "").is_err());
    assert_eq!(
        session_file(&dir, "a_B-1").unwrap(),
        PathBuf::from("/data/sessions/a_B-1.jsonl")
    );
}

#[test]
fn only_a_file_a_session_could_have_written_names_a_session() {
    assert_eq!(session_id_of("a_B-1.jsonl"), Some("a_B-1"));
    for other in ["a.b.jsonl", ".jsonl", "notes.txt", "a.jsonl.bak"] {
        assert_eq!(session_id_of(other), None, "{other}");
    }
}

/// A file as the TypeScript haest writes it (shapes from its session-store tests and
/// host), plus a record kind a host keeps for itself and a torn final line.
fn ts_fixture() -> String {
    [
        r#"{"type":"header","v":1,"sessionId":"abc","conversationId":"c1","cwd":"/x","createdAt":"2026-09-28T10:00:00.000Z"}"#,
        r#"{"type":"message","t":1727517600001,"message":{"role":"user","content":"one"}}"#,
        r#"{"type":"config","t":1727517600002,"model":"openrouter:m","effort":"high"}"#,
        r#"{"type":"message","t":1727517600003,"message":{"role":"assistant","content":[{"type":"text","text":"reading"},{"type":"tool_call","id":"c1","name":"Read","input":{"path":"a.txt"}}],"provider":"openrouter","model":"m","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"reasoning":0},"stopReason":"tool_use"}}"#,
        "{\"type\":\"message\",\"t\":1727517600004,\"message\":{\"role\":\"tool\",\"toolCallId\":\"c1\",\"toolName\":\"Read\",\"content\":\"a\u{2028}b\",\"isError\":false}}",
        r#"{"type":"ledger","t":1727517600005,"ledger":{"usd":0.25,"unpricedTokens":120,"caps":{"maxUsd":5}}}"#,
        r#"{"type":"tasks","t":1727517600006,"tasks":[{"id":"1","subject":"write schema","status":"completed","blockedBy":[],"blocks":["2"],"updatedAt":6,"completedAt":6},{"id":"2","subject":"write api","status":"pending","blockedBy":["1"],"blocks":[],"updatedAt":6}]}"#,
        r#"{"type":"host_note","t":1727517600007,"note":"a record the host keeps for itself"}"#,
        r#"{"type":"config","t":1727517600008,"effort":null}"#,
        r#"{"type":"message","t":1727517600009,"message":{"role":"user","content":"two"}}"#,
        r#"{"type":"compaction","t":1727517600010,"summary":"SUM","kept":3}"#,
        r#"{"type":"message","t":1727517600011,"message":{"role":"user","content":"thr"#,
    ]
    .join("\n")
}

#[test]
fn a_typescript_haest_session_file_replays() {
    let s = replay(&ts_fixture());
    assert_eq!(s.corrupt_lines, 1);
    assert_eq!(s.messages.len(), 4);
    assert!(texts(&s.messages)[0].contains("SUM"));
    let Message::Assistant(a) = &s.messages[1] else {
        panic!("assistant expected")
    };
    assert_eq!(a.tool_calls().count(), 1);
    let Message::Tool(t) = &s.messages[2] else {
        panic!("tool result expected")
    };
    assert_eq!(t.content, "a\u{2028}b");
    assert_eq!(texts(&s.messages)[3], "two");
    assert_eq!(s.model_spec.as_deref(), Some("openrouter:m"));
    assert_eq!(s.effort, EffortChange::ModelDefault);
    let ledger = s.ledger.unwrap();
    assert_eq!((ledger.usd, ledger.unpriced_tokens), (0.25, 120));
    assert_eq!(ledger.caps.and_then(|c| c.max_usd), Some(5.0));
    let tasks = s.tasks.unwrap();
    assert_eq!(tasks[1].blocked_by, ["1"]);
    assert_eq!(tasks[0].completed_at, Some(6));
}

#[test]
fn a_spend_replays_to_the_same_number_the_typescript_haest_wrote() {
    // A real TS session's ledger: one bit past what a fast float parser keeps.
    let line =
        r#"{"type":"ledger","t":1,"ledger":{"usd":0.0072744056000000005,"unpricedTokens":0}}"#;
    let usd = replay(line).ledger.unwrap().usd;
    assert_eq!(usd.to_bits(), 0.0072744056000000005_f64.to_bits());
}

#[test]
fn a_foreign_record_is_skipped_and_a_broken_haest_record_is_corrupt() {
    let text = [
        r#"{"type":"host_note","x":1}"#,
        r#"{"no_type":true}"#,
        r#"{"type":"message","t":1}"#,
        "42",
        r#"{"type":"message","t":1.5,"message":{"role":"user","content":"ok"}}"#,
    ]
    .join("\n");
    let s = replay(&text);
    assert_eq!(texts(&s.messages), ["ok"]);
    assert_eq!(s.corrupt_lines, 2);
}

#[test]
fn records_serialize_in_the_typescript_shape() {
    let cases = [
        (
            SessionRecord::from(SessionHeader {
                session_id: "abc".into(),
                conversation_id: "c1".into(),
                cwd: "/x".into(),
                created_ms: 0,
            }),
            r#"{"type":"header","v":1,"sessionId":"abc","conversationId":"c1","cwd":"/x","createdAt":"1970-01-01T00:00:00.000Z"}"#,
        ),
        (
            SessionRecord::Config {
                t: 2,
                model_spec: Some("openrouter:m".into()),
                effort: EffortChange::To(Effort::High),
            },
            r#"{"type":"config","t":2,"model":"openrouter:m","effort":"high"}"#,
        ),
        (
            SessionRecord::Config {
                t: 3,
                model_spec: None,
                effort: EffortChange::ModelDefault,
            },
            r#"{"type":"config","t":3,"effort":null}"#,
        ),
        (
            SessionRecord::Config {
                t: 5,
                model_spec: Some("openrouter:m".into()),
                effort: EffortChange::Unchanged,
            },
            r#"{"type":"config","t":5,"model":"openrouter:m"}"#,
        ),
        (
            SessionRecord::Compaction {
                t: 4,
                summary: "SUM".into(),
                kept: 2,
            },
            r#"{"type":"compaction","t":4,"summary":"SUM","kept":2}"#,
        ),
    ];
    for (record, ts) in cases {
        assert_eq!(serde_json::to_string(&record).unwrap(), ts);
        assert_eq!(serde_json::from_str::<SessionRecord>(ts).unwrap(), record);
    }
}
