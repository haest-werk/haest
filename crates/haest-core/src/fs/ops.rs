//! What a tool or a host does over any [`Fs`]: make a directory, read a file
//! that may not be there, tell binary from text.

use std::{io, path::Path};

use super::{FileKind, Fs};

/// Make sure a directory is at `path`: [`Fs::mkdir`] with `recursive`, not
/// called when the directory is already there (a file system that refuses
/// writes still says yes to one it has).
///
/// # Errors
///
/// `NotADirectory` when something other than a directory is at `path`, and
/// [`Fs::mkdir`]'s, such as a file where one of the parents should be.
pub async fn ensure_dir(fs: &dyn Fs, path: &Path) -> io::Result<()> {
    match fs.stat(path).await {
        Ok(stat) if stat.kind == FileKind::Dir => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is there and is not a directory", path.display()),
        )),
        Err(_) => fs.mkdir(path, true).await,
    }
}

/// A file a host may or may not have (a config file, `.mcp.json`): its text,
/// None when there is no such file -- nothing at the path, or a file where one
/// of its dirs should be. One read: no `exists` first. A file that is there
/// but cannot be read is an error, never a silent default. Strict UTF-8, like
/// [`Fs::read_to_string`].
///
/// # Errors
///
/// Any read error other than `NotFound` and `NotADirectory`, and
/// `InvalidData` for text that is not UTF-8.
pub async fn read_text_if_present(fs: &dyn Fs, path: &Path) -> io::Result<Option<String>> {
    match fs.read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Is `buf` binary? True when a NUL byte is in its first 8000 bytes, the way
/// git and grep guess it.
pub fn looks_binary(buf: &[u8]) -> bool {
    buf.iter().take(8000).any(|&b| b == 0)
}

/// A file's text with invalid UTF-8 replaced; None when it cannot be read.
/// For files haest reads if they are there (instruction files, skills, git's HEAD).
pub(crate) async fn read_lossy(fs: &dyn Fs, path: &Path) -> Option<String> {
    let bytes = fs.read(path).await.ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{}: no such file or directory", path.display()),
    )
}
