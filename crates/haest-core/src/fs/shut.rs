//! An [`Fs`] that refuses everything.

use std::{io, path::Path};

use async_trait::async_trait;

use super::{DirEntry, Fs, FsStat};

/// An [`Fs`] that refuses everything: a workspace with no files at all
/// ([`ShutWorkspace`](crate::ShutWorkspace)).
///
/// Every call fails with `PermissionDenied` (but [`Fs::chmod`], which has
/// nothing to set), so [`Fs::exists`] is false for every path.
#[derive(Debug, Default)]
pub struct ShutFs;

fn shut() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "this workspace has no file system",
    )
}

#[async_trait]
impl Fs for ShutFs {
    async fn read(&self, _: &Path) -> io::Result<Vec<u8>> {
        Err(shut())
    }
    async fn write(&self, _: &Path, _: &[u8]) -> io::Result<()> {
        Err(shut())
    }
    async fn append(&self, _: &Path, _: &[u8]) -> io::Result<()> {
        Err(shut())
    }
    async fn stat(&self, _: &Path) -> io::Result<FsStat> {
        Err(shut())
    }
    async fn read_dir(&self, _: &Path) -> io::Result<Vec<DirEntry>> {
        Err(shut())
    }
    async fn mkdir(&self, _: &Path, _: bool) -> io::Result<()> {
        Err(shut())
    }
    async fn remove(&self, _: &Path, _: bool) -> io::Result<()> {
        Err(shut())
    }
    async fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(shut())
    }
}
