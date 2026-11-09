//! The sandbox's mount table: every path identical to the real one.
//!
//! ```text
//!   <cwd>        copy-on-write overlay of the project: reads from disk, writes in memory
//!   <writable>   real read-write dirs the host must persist (the spill dir)
//!   <readable>   real read-only dirs (skill directories)
//!   everything else  an in-memory scratch disk (/tmp and friends)
//! ```

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use bashkit::{
    FileSystem, FsLimits, InMemoryFs, MountableFs, OverlayFs, PosixFs, RealFs, RealFsMode,
};
use kern_core::fs::normalize;

use crate::{overlay::OverlayChanges, real::RealFs as Disk};

/// What the project overlay may hold in memory before writes fail.
const OVERLAY_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct SandboxMounts {
    /// The project: overlaid, never written.
    pub cwd: PathBuf,
    /// Real directories the sandbox may write through to. Must already exist.
    pub writable: Vec<PathBuf>,
    /// Real directories the sandbox may read.
    pub readable: Vec<PathBuf>,
}

impl SandboxMounts {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Default::default()
        }
    }
    pub fn writable(mut self, dir: impl Into<PathBuf>) -> Self {
        self.writable.push(dir.into());
        self
    }
    pub fn readable(mut self, dir: impl Into<PathBuf>) -> Self {
        self.readable.push(dir.into());
        self
    }
}

fn inside(child: &Path, parent: &Path) -> bool {
    normalize(child).starts_with(normalize(parent))
}

/// Mount points may not nest: the first claim on a subtree wins.
fn disjoint(paths: &[PathBuf], taken: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        let clash = taken
            .iter()
            .chain(&out)
            .any(|t| inside(p, t) || inside(t, p));
        if !clash {
            out.push(normalize(p));
        }
    }
    out
}

/// No limit but the overlay's memory, and the path checks.
fn limits(bytes: u64) -> FsLimits {
    let d = FsLimits::default();
    FsLimits::unlimited()
        .max_total_bytes(bytes)
        .max_path_depth(d.max_path_depth)
        .max_filename_length(d.max_filename_length)
        .max_path_length(d.max_path_length)
}

async fn real_dir(dir: &Path, mode: RealFsMode) -> io::Result<Arc<dyn FileSystem>> {
    Ok(Arc::new(PosixFs::new(RealFs::open(dir, mode).await?)))
}

/// What bashkit seeds every fresh in-memory layer with -- an overlay's upper
/// layer included, where they would show up as phantom project directories.
const SEEDED: [&str; 3] = ["/dev", "/home", "/tmp"];

/// The copy-on-write overlay of `cwd` and its change ledger.
pub(crate) async fn project_overlay(cwd: &Path) -> io::Result<OverlayChanges> {
    let lower = real_dir(cwd, RealFsMode::ReadOnly).await?;
    let overlay = OverlayFs::with_limits(lower, limits(OVERLAY_MEMORY_BYTES));
    for dir in SEEDED {
        // Straight on the upper layer: no whiteout, the project's own shows through.
        let _ = overlay.upper().remove(Path::new(dir), true).await;
    }
    Ok(OverlayChanges::new(Arc::new(overlay), Arc::new(Disk), cwd))
}

/// The whole sandbox file system: `changes`' recorder at `cwd`, the real
/// writable and readable dirs, and a scratch disk under everything else.
pub(crate) async fn sandbox_fs(
    m: &SandboxMounts,
    changes: &OverlayChanges,
) -> io::Result<Arc<MountableFs>> {
    let base = Arc::new(InMemoryFs::with_limits(limits(OVERLAY_MEMORY_BYTES)));
    let cwd = normalize(&m.cwd);
    let writable = disjoint(&m.writable, std::slice::from_ref(&cwd));
    let taken: Vec<PathBuf> = std::iter::once(cwd.clone())
        .chain(writable.iter().cloned())
        .collect();
    let readable = disjoint(&m.readable, &taken);

    let mut table: Vec<(PathBuf, Arc<dyn FileSystem>)> = vec![(cwd, changes.recorder())];
    for dir in writable {
        let fs = real_dir(&dir, RealFsMode::ReadWrite).await?;
        table.push((dir, fs));
    }
    for dir in readable {
        let fs = real_dir(&dir, RealFsMode::ReadOnly).await?;
        table.push((dir, fs));
    }

    let fs = Arc::new(MountableFs::new(base.clone()));
    for (point, mounted) in table {
        // The scratch disk holds every mount point's ancestors, so `ls /` and
        // `cd ..` walk to a mount the way they would on the real disk.
        base.add_dir(&point, 0o755);
        fs.mount(&point, mounted)
            .map_err(crate::bridge::io_from_bashkit)?;
    }
    Ok(fs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounts_never_nest_the_first_claim_wins() {
        let p = |s: &str| PathBuf::from(s);
        let got = disjoint(
            &[
                p("/proj/sub"),
                p("/spill"),
                p("/spill/x"),
                p("/"),
                p("/other"),
            ],
            &[p("/proj")],
        );
        assert_eq!(got, [p("/spill"), p("/other")]);
    }
}
