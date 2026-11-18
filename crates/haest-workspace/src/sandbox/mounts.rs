//! The sandbox's mount table: every path identical to the real one (the table
//! is on [`SandboxMounts`]).

use std::{
    fmt, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use bashkit::{
    FileSystem, FsLimits, InMemoryFs, MountableFs, OverlayFs, PosixFs, RealFs, RealFsMode,
};
use haest_core::fs::normalize;

use super::spelled_fs::SpelledFs;
use crate::{overlay::OverlayChanges, real::RealFs as Disk};

/// What the project overlay (and, apart, the scratch disk) may hold in memory
/// before writes fail.
const MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// Which real directories the default sandbox
/// ([`SandboxWorkspace::open`](crate::SandboxWorkspace::open)) shows, and how.
///
/// Every directory is mounted at its own path, so a path means the same inside
/// the sandbox and out. A relative one is taken from this process's current
/// directory when the sandbox opens. Dots are resolved by the path alone, as
/// `cd ..` does in a shell: `link/..` is the directory that holds `link`,
/// wherever `link` points.
///
/// ```text
///   cwd         copy-on-write overlay of the project: reads from disk, writes in memory
///   writable    real read-write dirs the host must persist (the spill dir)
///   readable    real read-only dirs (skill directories)
///   any other   an in-memory scratch disk (/tmp and friends)
/// ```
///
/// Mounts may nest, as on a real mount table: a path belongs to the deepest
/// mount that holds it. A writable dir inside the project writes through to
/// the disk while the rest of the project stays copy-on-write; a project
/// inside a readable dir stays the overlay, and the readable dir's other files
/// show around it. The same directory listed twice keeps its first role
/// (`cwd`, then writable, then readable).
///
/// ```
/// use haest_workspace::SandboxMounts;
///
/// let mounts = SandboxMounts::new("/work/project")
///     .with_writable("/var/spill")
///     .with_readable("/home/me/.claude/skills");
/// assert_eq!(mounts.writable.len(), 1);
/// ```
#[derive(Debug, Clone, Default)]
pub struct SandboxMounts {
    /// The project: overlaid, never written. A relative path is taken from
    /// the process's current directory when the sandbox opens.
    pub cwd: PathBuf,
    /// Real directories the sandbox may write through to. Must already exist.
    /// A relative path is taken from the process's current directory when the
    /// sandbox opens.
    pub writable: Vec<PathBuf>,
    /// Real directories the sandbox may read. Must already exist. A relative
    /// path is taken from the process's current directory when the sandbox
    /// opens.
    pub readable: Vec<PathBuf>,
}

impl SandboxMounts {
    /// The project at `cwd`, and nothing else real.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Default::default()
        }
    }
    /// Add a real directory the sandbox writes through to, at once and for
    /// good (it is not one of the changes to review).
    #[must_use]
    pub fn with_writable(mut self, dir: impl Into<PathBuf>) -> Self {
        self.writable.push(dir.into());
        self
    }
    /// Add a real directory the sandbox may read and never write.
    #[must_use]
    pub fn with_readable(mut self, dir: impl Into<PathBuf>) -> Self {
        self.readable.push(dir.into());
        self
    }

    /// Every directory as one absolute path, which the sandbox opens, mounts
    /// and applies to: a relative one from this process's current directory,
    /// dots resolved by the path alone (`link/..` is where `link` sits). An
    /// empty path is refused.
    pub(crate) fn absolute(self) -> Result<Self, OpenSandboxError> {
        let abs = |dir: PathBuf| {
            std::path::absolute(&dir)
                .map(|a| normalize(&a))
                .map_err(|e| OpenSandboxError::new(&dir, e))
        };
        Ok(Self {
            cwd: abs(self.cwd)?,
            writable: self
                .writable
                .into_iter()
                .map(abs)
                .collect::<Result<_, _>>()?,
            readable: self
                .readable
                .into_iter()
                .map(abs)
                .collect::<Result<_, _>>()?,
        })
    }
}

/// What a mounted directory is in the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The project: the recording copy-on-write overlay.
    Project,
    /// Real, read-write.
    Writable,
    /// Real, read-only.
    Readable,
}

/// Every directory to mount, once each, from mounts made
/// [`absolute`](SandboxMounts::absolute): the same directory listed twice
/// keeps its first role (`cwd`, then writable, then readable). Nesting is the
/// mount table's to resolve (the deepest mount holding a path wins).
fn roles(m: &SandboxMounts) -> Vec<(PathBuf, Role)> {
    let claims = std::iter::once((&m.cwd, Role::Project))
        .chain(m.writable.iter().map(|d| (d, Role::Writable)))
        .chain(m.readable.iter().map(|d| (d, Role::Readable)));
    let mut out: Vec<(PathBuf, Role)> = Vec::new();
    for (dir, role) in claims {
        if !out.iter().any(|(taken, _)| taken == dir) {
            out.push((dir.clone(), role));
        }
    }
    out
}

/// No limit but the memory, and the path checks.
fn limits() -> FsLimits {
    let d = FsLimits::default();
    FsLimits::unlimited()
        .max_total_bytes(MEMORY_BYTES)
        .max_path_depth(d.max_path_depth)
        .max_filename_length(d.max_filename_length)
        .max_path_length(d.max_path_length)
}

/// A directory of the sandbox's that did not open
/// ([`SandboxWorkspace::open`](crate::SandboxWorkspace::open)): the project
/// or a mount, named, with the I/O error as its [`source`](std::error::Error::source).
#[derive(Debug)]
pub struct OpenSandboxError {
    path: PathBuf,
    source: io::Error,
}

impl OpenSandboxError {
    fn new(path: &Path, source: io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            source,
        }
    }

    /// The directory that did not open (empty when it was an empty path).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What opening it hit (`NotFound`, `PermissionDenied`, ...).
    pub fn kind(&self) -> io::ErrorKind {
        self.source.kind()
    }
}

impl fmt::Display for OpenSandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.as_os_str().is_empty() {
            return f.write_str("opening an empty path for the sandbox");
        }
        write!(f, "opening {} for the sandbox", self.path.display())
    }
}

impl std::error::Error for OpenSandboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Of the same kind, the dir named: `?` in a function that returns an
/// [`io::Result`].
impl From<OpenSandboxError> for io::Error {
    fn from(e: OpenSandboxError) -> Self {
        io::Error::new(e.kind(), e)
    }
}

/// `dir` from the real disk, the error naming it.
async fn real_dir(dir: &Path, mode: RealFsMode) -> Result<Arc<dyn FileSystem>, OpenSandboxError> {
    let opened = RealFs::open(dir, mode).await;
    let fs = opened.map_err(|e| OpenSandboxError::new(dir, e))?;
    Ok(Arc::new(PosixFs::new(fs)))
}

/// What bashkit seeds every fresh in-memory layer with -- an overlay's upper
/// layer included, where they would show up as phantom project directories.
const SEEDED: [&str; 3] = ["/dev", "/home", "/tmp"];

/// The copy-on-write overlay of `cwd` and its change ledger. Under it, the
/// project is found only as the disk spells each path.
pub(crate) async fn project_overlay(cwd: &Path) -> Result<OverlayChanges, OpenSandboxError> {
    let lower = real_dir(cwd, RealFsMode::ReadOnly).await?;
    let lower: Arc<dyn FileSystem> = Arc::new(SpelledFs::new(lower, cwd));
    let overlay = OverlayFs::with_limits(lower, limits());
    for dir in SEEDED {
        // Straight on the upper layer: no whiteout, the project's own shows through.
        let _ = overlay.upper().remove(Path::new(dir), true).await;
    }
    Ok(OverlayChanges::new(Arc::new(overlay), Arc::new(Disk), cwd))
}

/// The whole sandbox file system, over mounts made
/// [`absolute`](SandboxMounts::absolute): `changes`' recorder at `cwd`, the
/// real writable and readable dirs, and a scratch disk under everything else.
pub(crate) async fn sandbox_fs(
    m: &SandboxMounts,
    changes: &OverlayChanges,
) -> Result<Arc<MountableFs>, OpenSandboxError> {
    let base = Arc::new(InMemoryFs::with_limits(limits()));
    let fs = Arc::new(MountableFs::new(base.clone()));
    for (point, role) in roles(m) {
        let mounted = match role {
            Role::Project => changes.recorder(),
            Role::Writable => real_dir(&point, RealFsMode::ReadWrite).await?,
            Role::Readable => real_dir(&point, RealFsMode::ReadOnly).await?,
        };
        // The scratch disk holds every mount point's ancestors, so `ls /` and
        // `cd ..` walk to a mount the way they would on the real disk.
        base.add_dir(&point, 0o755);
        fs.mount(&point, mounted)
            .map_err(|e| OpenSandboxError::new(&point, crate::bridge::io_from_bashkit(e)))?;
    }
    Ok(fs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_dirs_all_mount_and_a_dir_listed_twice_keeps_its_first_role() {
        let p = |s: &str| PathBuf::from(s);
        let m = SandboxMounts::new("/proj")
            .with_writable("/proj/spill")
            .with_writable("/proj/")
            .with_writable("/proj/./x/..")
            .with_readable("/home")
            .with_readable("/proj/spill");
        assert_eq!(
            roles(&m.absolute().unwrap()),
            [
                (p("/proj"), Role::Project),
                (p("/proj/spill"), Role::Writable),
                (p("/home"), Role::Readable),
            ]
        );
    }
}
