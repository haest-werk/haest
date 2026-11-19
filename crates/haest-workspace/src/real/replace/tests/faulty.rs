//! The real disk with a fault in it, and what the apply did on it.

use std::{
    fs::{self, Permissions},
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use haest_core::fs::{DirEntry, Fs, FsStat};
use tokio::sync::Notify;

use crate::RealFs;

/// What goes wrong on the disk beside `real`, the file the apply writes.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Fault {
    /// A write of bytes anywhere but `real` is left to the runtime's pool,
    /// which starts it late, as it starts `tokio::fs::write` under load;
    /// dropping the write's future does not stop it.
    SlowWrite,
    /// A rename onto `real` fails as one onto a mount point does: the file
    /// is bind-mounted into the project on its own.
    BusyRename,
    /// A name made in `real`'s directory fails with this OS error:
    /// `ENAMETOOLONG` where `real`'s path is just short of the system's
    /// cap, `ENOSPC` on a disk with no room for even an empty file.
    NewName(i32),
    /// A directory made beside `real` passes its group on (setgid), as
    /// Linux makes every directory in one that does: a team's directory.
    Setgid,
    /// Every chmod fails with this OS error: it makes files, but sets no
    /// mode. `EPERM` on a share mounted for one fixed owner the run is
    /// not, `ENOSYS` on a FUSE file system that has no setattr.
    NoModes(i32),
}

/// The real disk, with `fault` in it.
pub(super) struct Faulty {
    real: PathBuf,
    fault: Option<Fault>,
    /// A write past this many bytes writes that many, then fails as a full
    /// disk does.
    pub(super) room: AtomicUsize,
    /// Told once a [`Fault::SlowWrite`] is in the runtime's hands.
    pub(super) began: Notify,
    /// The permission bits of each file beside `real` as bytes went into
    /// it; None where it was not there yet.
    pub(super) filled: Mutex<Vec<Option<u32>>>,
    /// Each mode put on a directory that passes its group on.
    pub(super) setgid_chmods: Mutex<Vec<u32>>,
    /// The mode of the directory each new file beside `real` was written
    /// in, as it was written (the empty create and the fill).
    pub(super) new_file_dirs: Mutex<Vec<u32>>,
}

impl Faulty {
    pub(super) fn new(real: &Path, fault: Option<Fault>) -> Arc<Self> {
        Arc::new(Self {
            real: real.to_path_buf(),
            fault,
            room: AtomicUsize::new(usize::MAX),
            began: Notify::new(),
            filled: Mutex::new(Vec::new()),
            setgid_chmods: Mutex::new(Vec::new()),
            new_file_dirs: Mutex::new(Vec::new()),
        })
    }

    /// A name the apply makes beside `real`, refused as [`Fault::NewName`]
    /// says.
    fn refuses_name(&self, p: &Path) -> io::Result<()> {
        match self.fault {
            Some(Fault::NewName(code)) if p != self.real && p.parent() == self.real.parent() => {
                Err(io::Error::from_raw_os_error(code))
            }
            _ => Ok(()),
        }
    }

    /// Note who may reach the file at `p`, beside `real`, as `d` goes in,
    /// and who may enter the new-file directory it is in.
    fn note_fill(&self, p: &Path, d: &[u8]) {
        if p != self.real && !d.is_empty() {
            let mode = fs::symlink_metadata(p).ok().map(|m| m.permissions().mode());
            self.filled.lock().unwrap().push(mode.map(|m| m & 0o7777));
        }
        if let Some(dir) = p.parent().filter(|d| d.parent() == self.real.parent())
            && let Ok(m) = fs::symlink_metadata(dir)
        {
            let mode = m.permissions().mode() & 0o7777;
            self.new_file_dirs.lock().unwrap().push(mode);
        }
    }

    /// Under [`Fault::Setgid`], the directory `p` just made beside `real`
    /// gets the setgid bit, as Linux gives it whatever mode it was made
    /// with.
    fn pass_group_on(&self, p: &Path) -> io::Result<()> {
        if self.fault == Some(Fault::Setgid) && p.parent() == self.real.parent() {
            let mode = fs::symlink_metadata(p)?.permissions().mode();
            fs::set_permissions(p, Permissions::from_mode(mode | 0o2000))?;
        }
        Ok(())
    }
}

/// Whether `p` is a directory that passes its group on.
fn setgid_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && m.permissions().mode() & 0o2000 != 0)
}

#[async_trait]
impl Fs for Faulty {
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.refuses_name(p)?;
        self.note_fill(p, d);
        let room = self.room.load(Ordering::SeqCst);
        if d.len() > room {
            RealFs.write(p, &d[..room]).await?;
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        }
        if self.fault == Some(Fault::SlowWrite) && p != self.real && !d.is_empty() {
            let (p, d) = (p.to_path_buf(), d.to_vec());
            let write = tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(100));
                fs::write(p, d)
            });
            self.began.notify_one();
            return write.await.map_err(io::Error::other)?;
        }
        RealFs.write(p, d).await
    }
    /// The real disk's whole write, over this disk, so its faults apply.
    async fn write_whole(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        crate::real::replace::put_file(self, p, d, crate::real::replace::Name::Asked).await
    }
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        RealFs.read(p).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        RealFs.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        RealFs.stat(p).await
    }
    async fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        RealFs.read_link(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        RealFs.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.refuses_name(p)?;
        RealFs.create_dir(p).await?;
        self.pass_group_on(p)
    }
    async fn create_private_dir(&self, p: &Path) -> io::Result<()> {
        self.refuses_name(p)?;
        RealFs.create_private_dir(p).await?;
        self.pass_group_on(p)
    }
    async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        RealFs.create_dir_all(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        RealFs.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        RealFs.remove_all(p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        if self.fault == Some(Fault::BusyRename) && b == self.real {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        RealFs.rename(a, b).await
    }
    async fn chmod(&self, p: &Path, mode: u32) -> io::Result<()> {
        if setgid_dir(p) {
            self.setgid_chmods.lock().unwrap().push(mode);
        }
        if let Some(Fault::NoModes(code)) = self.fault {
            return Err(io::Error::from_raw_os_error(code));
        }
        RealFs.chmod(p, mode).await
    }
}
