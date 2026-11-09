//! A workspace for tests: files in a [`MemFs`], every command answered by a
//! closure and recorded. kern-workspace owns the real disk and the real bash;
//! these tests defend what the TOOLS do with a workspace, not the workspace.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use kern_core::{
    fs::{Fs, MemFs},
    tool::{JsonMap, ToolContext},
    workspace::{ExecOptions, ExecResult, Workspace, WorkspaceMode},
};
use serde_json::Value;

/// The working directory every test context runs in.
pub const CWD: &str = "/p";

/// One command a tool ran: `shell` = a command line, else a program and its argv.
#[derive(Debug, Clone)]
pub struct Call {
    pub shell: bool,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

type Answer = Box<dyn Fn(&Call) -> ExecResult + Send + Sync>;

pub struct TestWorkspace {
    pub mem: Arc<MemFs>,
    answer: Answer,
    calls: Mutex<Vec<Call>>,
}

impl TestWorkspace {
    /// Files only; every command exits 127 (no such program).
    pub fn with_files<P: AsRef<Path>, D: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, D)>,
    ) -> Arc<Self> {
        Self::answering(files, |_| ExecResult {
            exit_code: 127,
            ..Default::default()
        })
    }

    pub fn answering<P: AsRef<Path>, D: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, D)>,
        answer: impl Fn(&Call) -> ExecResult + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            mem: Arc::new(MemFs::with_files(files)),
            answer: Box::new(answer),
            calls: Mutex::default(),
        })
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// What the workspace says is in a file.
    pub async fn contents(&self, path: &str) -> String {
        self.mem.read_to_string(Path::new(path)).await.unwrap()
    }

    fn record(&self, shell: bool, argv: Vec<String>, opts: ExecOptions) -> ExecResult {
        let call = Call {
            shell,
            argv,
            cwd: opts.cwd,
            timeout: opts.timeout,
        };
        let out = (self.answer)(&call);
        self.calls.lock().unwrap().push(call);
        out
    }
}

#[async_trait]
impl Workspace for TestWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Sandbox
    }
    fn fs(&self) -> Arc<dyn Fs> {
        self.mem.clone()
    }
    async fn shell(&self, command: &str, opts: ExecOptions) -> ExecResult {
        self.record(true, vec![command.to_string()], opts)
    }
    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        self.record(false, argv.to_vec(), opts)
    }
    fn prompt_note(&self) -> String {
        String::new()
    }
}

pub fn ctx(ws: Arc<TestWorkspace>) -> ToolContext {
    ToolContext::new(CWD, ws)
}

pub fn input(v: Value) -> JsonMap {
    v.as_object().cloned().expect("a JSON object")
}

/// An exit-0 result with this stdout.
pub fn stdout(text: &str) -> ExecResult {
    ExecResult {
        stdout: text.to_string(),
        ..Default::default()
    }
}
