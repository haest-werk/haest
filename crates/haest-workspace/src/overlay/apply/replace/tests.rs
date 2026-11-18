//! The new file the apply puts in a file's place, on the real disk with a
//! fault in it: a full disk, a write the runtime is slow to start.

use std::{
    fs::{self, Metadata, Permissions},
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use haest_core::{
    fs::{DirEntry, Fs, FsRef, FsStat, MemFs},
    workspace::{ApplyFailureCode, Change, ChangeSet},
};
use tokio::sync::Notify;

use crate::{RealFs, bridge::BashkitFsOverHaest, overlay::OverlayChanges};

/// What goes wrong on the disk beside `real`, the file the apply writes.
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    /// A write of bytes anywhere but `real` is left to the runtime's pool,
    /// which starts it late, as it starts `tokio::fs::write` under load;
    /// dropping the write's future does not stop it.
    SlowWrite,
}

/// The real disk, with `fault` in it.
struct Faulty {
    real: PathBuf,
    fault: Option<Fault>,
    /// A write past this many bytes writes that many, then fails as a full
    /// disk does.
    room: AtomicUsize,
    /// Told once a [`Fault::SlowWrite`] is in the runtime's hands.
    began: Notify,
}

impl Faulty {
    fn new(real: &Path, fault: Option<Fault>) -> Arc<Self> {
        Arc::new(Self {
            real: real.to_path_buf(),
            fault,
            room: AtomicUsize::new(usize::MAX),
            began: Notify::new(),
        })
    }
}

#[async_trait]
impl Fs for Faulty {
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
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
        RealFs.create_dir(p).await
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
        RealFs.rename(a, b).await
    }
    async fn chmod(&self, p: &Path, mode: u32) -> io::Result<()> {
        RealFs.chmod(p, mode).await
    }
}

/// A project dir by its real path, holding `name` with `human` in it, and
/// the sandbox's edit of it to `sandbox`, applied to the disk `fault` makes.
async fn edited(
    name: &str,
    human: &str,
    sandbox: &str,
    fault: Option<Fault>,
) -> (tempfile::TempDir, PathBuf, Arc<Faulty>, OverlayChanges) {
    let t = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(t.path()).unwrap();
    let real = root.join(name);
    fs::write(&real, human).unwrap();
    let path = Path::new("/").join(name);
    let files = MemFs::from_files([(path.as_path(), human)]);
    let overlay = Arc::new(BashkitFsOverHaest::new(FsRef::new(files)));
    let disk = Faulty::new(&real, fault);
    let changes = OverlayChanges::new(overlay, disk.clone(), &root);
    let recorder = changes.recorder();
    recorder
        .write_file(&path, sandbox.as_bytes())
        .await
        .unwrap();
    (t, real, disk, changes)
}

fn conflicts(changes: &[Change]) -> Vec<(&str, bool)> {
    changes
        .iter()
        .map(|c| (c.path.as_str(), c.conflict))
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

/// Everything under `dir`, at any depth, without following a link.
fn under(dir: &Path) -> Vec<(PathBuf, Metadata)> {
    let mut all = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        let meta = fs::symlink_metadata(&path).unwrap();
        let deeper = meta.is_dir().then(|| under(&path));
        all.push((path, meta));
        all.extend(deeper.into_iter().flatten());
    }
    all
}

/// Neither the group nor anyone else may reach into it.
fn owner_only(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o077 == 0
}

/// The disk fills up while the apply writes the sandbox's `notes.txt`.
/// The write had emptied the human's file first: the change failed,
/// the human's text was gone, and since the disk then held neither
/// version, every later scan listed it as a conflict and every apply
/// skipped it.
#[tokio::test]
async fn a_write_the_full_disk_stops_leaves_the_humans_file_whole() {
    let human = "the human's notes\n".repeat(50);
    let sandbox = "the sandbox's longer notes\n".repeat(100);
    let (_t, real, disk, changes) = edited("notes.txt", &human, &sandbox, None).await;
    disk.room.store(64, Ordering::SeqCst);

    let report = changes.apply().await;
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|f| (f.change.path.as_str(), f.code))
        .collect();
    assert_eq!(failed, [("/notes.txt", ApplyFailureCode::WriteFailed)]);
    let on_disk = fs::read_to_string(&real).unwrap();
    assert!(
        on_disk == human,
        "the human's file: {} bytes",
        on_disk.len()
    );
    assert_eq!(
        names(real.parent().unwrap()),
        ["notes.txt"],
        "nothing left beside it"
    );
    assert_eq!(conflicts(&changes.list().await), [("/notes.txt", false)]);

    // Room is made: the next apply writes it.
    disk.room.store(usize::MAX, Ordering::SeqCst);
    let report = changes.apply().await;
    assert_eq!(conflicts(&report.applied), [("/notes.txt", false)]);
    let on_disk = fs::read_to_string(&real).unwrap();
    assert!(on_disk == sandbox, "the sandbox's: {} bytes", on_disk.len());
    assert!(changes.list().await.is_empty());
}

/// The apply is dropped (Ctrl+C at the haest command's `/apply`, a host's
/// timeout) while the runtime still holds the write of the new file's
/// bytes. The drop removed the new file, owner-only till then; the write
/// then made it again, open to everyone, with the sandbox's version of the
/// private `secret.env` in it, under a name its own ignore pattern missed.
#[tokio::test]
async fn an_apply_dropped_before_its_write_starts_leaves_no_copy_open_to_others() {
    let human = "TOKEN=the human's\n";
    let sandbox = "TOKEN=the sandbox's\n";
    let (_t, real, disk, changes) =
        edited("secret.env", human, sandbox, Some(Fault::SlowWrite)).await;
    fs::set_permissions(&real, Permissions::from_mode(0o600)).unwrap();

    let dropped = tokio::select! {
        biased;
        () = disk.began.notified() => true,
        _ = changes.apply() => false,
    };
    assert!(dropped, "the apply ended before its write began");
    // The write the runtime held lands, or finds nowhere to land.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let root = real.parent().unwrap();
    for (path, meta) in under(root).iter().filter(|(p, _)| *p != real) {
        let shown = path.display();
        assert!(owner_only(meta), "{shown} is open to others: {meta:?}");
        if meta.is_file() && fs::read(path).unwrap() == sandbox.as_bytes() {
            let dir = path.parent().unwrap();
            let own = dir != root && owner_only(&fs::metadata(dir).unwrap());
            assert!(own, "{shown} holds the sandbox's version");
        }
    }
    assert_eq!(fs::read_to_string(&real).unwrap(), human);
}
