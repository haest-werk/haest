//! Fakes for this crate's tests: a workspace over a [`MemFs`], a tool that counts
//! its calls and their peak concurrency, and a host that records what tools report.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use haest_core::{
    fs::{FileKind, Fs, FsRef, MemFs, resolve_path},
    tool::{InnerCallEvent, JsonMap, Tool, ToolContext, ToolError, ToolOutput, ToolServices},
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, Workspace, WorkspaceMode},
};
use serde_json::{Value, json};

pub(crate) const CWD: &str = "/p";

/// Files in memory. `rg ... -e PATTERN TARGET` is answered by a literal scan of
/// them (files with a match, the way `rg -l` prints them); any other command exits 127.
pub(crate) struct MemWorkspace {
    pub mem: Arc<MemFs>,
}

impl MemWorkspace {
    pub(crate) fn with_files<P: AsRef<Path>, D: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, D)>,
    ) -> Arc<Self> {
        Arc::new(Self {
            mem: Arc::new(MemFs::with_files(files)),
        })
    }

    pub(crate) async fn contents(&self, path: &str) -> Option<String> {
        self.mem.read_to_string(Path::new(path)).await.ok()
    }

    async fn files_under(&self, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = self.mem.read_dir(dir).await else {
            out.push(dir.to_path_buf());
            return;
        };
        for e in entries {
            let p = dir.join(&e.name);
            match e.kind {
                FileKind::Dir => Box::pin(self.files_under(&p, out)).await,
                _ => out.push(p),
            }
        }
    }

    async fn rg(&self, argv: &[String], cwd: &Path) -> ExecResult {
        let at = argv.iter().position(|a| a == "-e").unwrap_or(argv.len());
        let (Some(pattern), Some(target)) = (argv.get(at + 1), argv.get(at + 2)) else {
            return ExecResult {
                exit_code: 2,
                stderr: "bad argv".into(),
                ..Default::default()
            };
        };
        let base = resolve_path(cwd, target);
        let mut files = Vec::new();
        self.files_under(&base, &mut files).await;
        let mut hits = Vec::new();
        for f in files {
            let text = self.mem.read_to_string(&f).await.unwrap_or_default();
            if text.contains(pattern.as_str()) {
                let rel = f.strip_prefix(&base).unwrap_or(&f).display().to_string();
                hits.push(format!("{}/{rel}", target.trim_end_matches('/')));
            }
        }
        ExecResult {
            exit_code: if hits.is_empty() { 1 } else { 0 },
            stdout: hits.iter().map(|h| format!("{h}\n")).collect(),
            ..Default::default()
        }
    }
}

#[async_trait]
impl Workspace for MemWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Sandbox
    }
    fn fs(&self) -> FsRef {
        self.mem.clone().into()
    }
    async fn shell(&self, _: &str, _: ExecOptions) -> ExecResult {
        ExecResult {
            exit_code: EXIT_NOT_FOUND,
            ..Default::default()
        }
    }
    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        match argv.first().map(String::as_str) {
            Some("rg") => self.rg(argv, &opts.cwd).await,
            _ => ExecResult {
                exit_code: EXIT_NOT_FOUND,
                ..Default::default()
            },
        }
    }
    fn prompt_note(&self) -> String {
        String::new()
    }
}

pub(crate) fn ctx(ws: Arc<MemWorkspace>) -> ToolContext {
    ToolContext::new(CWD, ws)
}

pub(crate) fn input(v: Value) -> JsonMap {
    v.as_object().cloned().expect("a JSON object")
}

/// Records what tools report to their [`ToolServices`].
#[derive(Default)]
pub(crate) struct Recorder {
    pub events: Mutex<Vec<InnerCallEvent>>,
}

impl ToolServices for Recorder {
    fn on_inner_call(&self, ev: InnerCallEvent) {
        self.events.lock().unwrap().push(ev);
    }
}

/// A fake tool that counts calls and peak concurrency, answering `answer` after `ms`.
pub(crate) struct Probe {
    pub name: String,
    pub read_only: bool,
    pub ms: u64,
    pub answer: ToolOutput,
    calls: AtomicUsize,
    live: AtomicUsize,
    peak: AtomicUsize,
}

impl Probe {
    pub(crate) fn new(name: &str, read_only: bool, ms: u64) -> Self {
        Self {
            name: name.into(),
            read_only,
            ms,
            answer: ToolOutput::text("ok"),
            calls: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(crate) fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn description(&self) -> String {
        self.name.clone()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(self.ms)).await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        Ok(self.answer.clone())
    }
}
