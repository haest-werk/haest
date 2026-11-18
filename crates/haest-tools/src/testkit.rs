//! A workspace for tests: files in a [`MemFs`], every command answered by a
//! closure and recorded. haest-workspace owns the real disk and the real bash;
//! these tests defend what the TOOLS do with a workspace, not the workspace.

mod noted;

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
use haest_core::{
    fs::{DirEntry, Fs, FsRef, FsStat, MemFs},
    tool::{JsonMap, ToolContext},
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, ShutWorkspace, Workspace, WorkspaceMode},
};
use noted::{Noted, Writes};
use serde_json::Value;

pub(crate) use noted::Write;

/// The working directory every test context runs in.
pub(crate) const CWD: &str = "/p";

/// One command a tool ran: `shell` = a command line, else a program and its argv.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub shell: bool,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

type Answer = Box<dyn Fn(&Call) -> ExecResult + Send + Sync>;

pub(crate) struct TestWorkspace {
    pub mem: MemFs,
    /// Each write a tool asked the files for.
    writes: Writes,
    answer: Answer,
    calls: Mutex<Vec<Call>>,
}

impl TestWorkspace {
    /// Files only; every command exits 127 (no such program).
    pub(crate) fn from_files<P: AsRef<Path>, D: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, D)>,
    ) -> Arc<Self> {
        Self::answering(files, |_| ExecResult::failed("", EXIT_NOT_FOUND))
    }

    pub(crate) fn answering<P: AsRef<Path>, D: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, D)>,
        answer: impl Fn(&Call) -> ExecResult + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            mem: MemFs::from_files(files),
            writes: Writes::default(),
            answer: Box::new(answer),
            calls: Mutex::default(),
        })
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Each write a tool asked the files for, with its path, in order.
    pub(crate) fn writes(&self) -> Vec<(Write, PathBuf)> {
        self.writes.lock().unwrap().clone()
    }

    /// What the workspace says is in a file.
    pub(crate) async fn contents(&self, path: &str) -> String {
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
    fn fs(&self) -> FsRef {
        let (mem, writes) = (self.mem.clone(), self.writes.clone());
        FsRef::new(Noted { mem, writes })
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

pub(crate) fn ctx(ws: Arc<TestWorkspace>) -> ToolContext {
    ToolContext::new(CWD, ws)
}

/// For a tool that never touches a workspace (tasks, skills, agents, ToolSearch).
pub(crate) fn shut_ctx() -> ToolContext {
    ToolContext::new(CWD, Arc::new(ShutWorkspace))
}

pub(crate) fn input(v: Value) -> JsonMap {
    v.as_object().cloned().expect("a JSON object")
}

/// Files whose stat claims `size` bytes, with whole reads counted: what a tool
/// does with a file too big to hold, without one on disk.
pub(crate) struct ClaimedSizeFs {
    mem: MemFs,
    size: u64,
    pub whole_reads: AtomicUsize,
}

impl ClaimedSizeFs {
    pub(crate) fn new<P: AsRef<Path>>(
        files: impl IntoIterator<Item = (P, &'static str)>,
        size: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            mem: MemFs::from_files(files),
            size,
            whole_reads: AtomicUsize::new(0),
        })
    }

    pub(crate) fn reads(&self) -> usize {
        self.whole_reads.load(Ordering::SeqCst)
    }

    /// A context over these files, a sandboxed shell beside them.
    pub(crate) fn ctx(self: &Arc<Self>) -> ToolContext {
        let ws = haest_workspace::SandboxWorkspace::over(self.clone());
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
    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.mem.create_dir(path).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.mem.create_dir_all(path).await
    }
    async fn remove(&self, path: &Path) -> io::Result<()> {
        self.mem.remove(path).await
    }
    async fn remove_all(&self, path: &Path) -> io::Result<()> {
        self.mem.remove_all(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.mem.rename(from, to).await
    }
}
