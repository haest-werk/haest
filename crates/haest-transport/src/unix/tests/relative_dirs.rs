//! A control dir named relative to the reader's working directory (`haest ctl
//! --control-dir ctl`, a program run from its project) is the same dir as
//! its absolute path: the sessions in it are found and reached.

use std::path::{Component, Path, PathBuf};

use haest_wire::{Origin, Request};

use super::{SocketDiscovery, UnixSocket, connect_session, running_sessions};
use crate::{
    Grant,
    testing::{answers, hub, session},
};

/// A dir under /tmp (short enough for a socket path, also when spelled
/// relative to a deep working directory); gone at the end.
struct TmpDir(PathBuf);

impl TmpDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(format!("/tmp/hx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        TmpDir(dir)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `abs` spelled relative to the working directory: a `..` per component of
/// it, then `abs` below the root.
fn relative(abs: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let up = cwd
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .map(|_| Component::ParentDir);
    let below_root = abs
        .components()
        .filter(|c| !matches!(c, Component::RootDir));
    let path: PathBuf = up
        .map(|c| c.as_os_str().to_owned())
        .chain(below_root.map(|c| c.as_os_str().to_owned()))
        .collect();
    assert!(path.is_relative());
    path
}

/// Found in haest-glass: a reader given a relative control dir listed
/// nothing, since a discovery file's socket (absolute) never equalled the
/// path beside the file as the reader spelled it.
#[tokio::test]
async fn a_session_is_found_and_reached_through_a_relative_control_dir() {
    let dir = TmpDir::new("rel-read");
    let (s, _) = session(&answers(&[]), hub());
    let _attached = UnixSocket::new(&dir.0)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();
    let rel = relative(&dir.0);

    let found = running_sessions(&rel).await;
    assert_eq!(found.len(), 1, "nothing found in {}", rel.display());
    assert_eq!(found[0].discovery.sid, "s1");
    let mut peer = connect_session(&rel, "s1").await.unwrap();
    let now = peer.call(Request::Snapshot, |_| {}).await.unwrap().unwrap();
    assert_eq!(now["model"], "scripted:m");
}

/// A haest given a relative control dir writes its socket's absolute path in
/// the discovery file: a reader in another directory can tell where it is.
#[tokio::test]
async fn a_relative_control_dir_writes_an_absolute_socket_path() {
    let dir = TmpDir::new("rel-write");
    let (s, _) = session(&answers(&[]), hub());
    let rel = relative(&dir.0);
    let _attached = UnixSocket::new(&rel)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();

    let file = std::fs::read(dir.0.join("s1.json")).unwrap();
    let found: SocketDiscovery = serde_json::from_slice(&file).unwrap();
    assert!(found.socket.is_absolute(), "{}", found.socket.display());
    assert_eq!(running_sessions(&dir.0).await.len(), 1);
}
