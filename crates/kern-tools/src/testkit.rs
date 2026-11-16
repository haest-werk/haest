//! A workspace for tests: files in a [`MemFs`], every command answered by a
//! closure and recorded. kern-workspace owns the real disk and the real bash;
//! these tests defend what the TOOLS do with a workspace, not the workspace.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use kern_core::{
    fs::{DirEntry, Fs, FsStat, MemFs},
    tool::{JsonMap, ToolContext},
    workspace::{ExecOptions, ExecResult, ShutWorkspace, Workspace, WorkspaceMode},
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

/// For a tool that never touches a workspace (tasks, skills, agents, ToolSearch).
pub fn shut_ctx() -> ToolContext {
    ToolContext::new(CWD, Arc::new(ShutWorkspace))
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

/// Files whose stat claims `size` bytes, with whole reads counted: what a tool
/// does with a file too big to hold, without one on disk.
pub struct ClaimedSizeFs {
    mem: MemFs,
    size: u64,
    pub whole_reads: AtomicUsize,
}

impl ClaimedSizeFs {
    pub fn new<P: AsRef<Path>>(
        files: impl IntoIterator<Item = (P, &'static str)>,
        size: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            mem: MemFs::with_files(files),
            size,
            whole_reads: AtomicUsize::new(0),
        })
    }

    pub fn reads(&self) -> usize {
        self.whole_reads.load(Ordering::SeqCst)
    }

    /// A context over these files, a sandboxed shell beside them.
    pub fn ctx(self: &Arc<Self>) -> ToolContext {
        let ws = kern_workspace::SandboxWorkspace::over(self.clone());
        ToolContext::new(CWD, Arc::new(ws))
    }
}

#[async_trait]
impl Fs for ClaimedSizeFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.whole_reads.fetch_add(1, Ordering::SeqCst);
        self.mem.read(path).await
    }
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.mem.write(path, data).await
    }
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.mem.append(path, data).await
    }
    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        let mut stat = self.mem.stat(path).await?;
        if stat.is_file() {
            stat.size = self.size;
        }
        Ok(stat)
    }
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.mem.read_dir(path).await
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()> {
        self.mem.mkdir(path, recursive).await
    }
    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        self.mem.remove(path, recursive).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.mem.rename(from, to).await
    }
}
