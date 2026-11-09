//! The two adapters between kern's [`Fs`](kern_core::fs::Fs) and bashkit's
//! `FileSystem` (PLAN.md D4), so the tools and the simulated shell share ONE
//! object: a file the agent writes with Write is the file `cat` shows.
//!
//! - [`KernFsOverBashkit`]: the sandbox's file system as a kern `Fs` (what the tools see).
//! - [`BashkitFsOverKern`]: any kern `Fs` as a bashkit file system, so a host's own
//!   storage gets a sandboxed bash.

mod bashkit_over_kern;
mod kern_over_bashkit;

pub use bashkit_over_kern::BashkitFsOverKern;
pub use kern_over_bashkit::KernFsOverBashkit;

use std::io::{self, ErrorKind};

/// bashkit -> io. bashkit reports some POSIX failures as message-only `Other`
/// errors ("is a directory"); they get their real kind back.
pub(crate) fn io_from_bashkit(e: bashkit::Error) -> io::Error {
    let e = match e {
        bashkit::Error::Io(e) => e,
        other => return io::Error::other(other.to_string()),
    };
    if e.kind() != ErrorKind::Other {
        return e;
    }
    let kind = match e.to_string().as_str() {
        "is a directory" => ErrorKind::IsADirectory,
        "not a directory" => ErrorKind::NotADirectory,
        "directory not empty" => ErrorKind::DirectoryNotEmpty,
        _ => return e,
    };
    io::Error::new(kind, e.to_string())
}

/// io -> bashkit, in the shapes bashkit's own layers use, so a builtin prints
/// `cat: f: No such file or directory` the way it does for its own files.
pub(crate) fn bashkit_from_io(e: io::Error) -> bashkit::Error {
    let conventional = match e.kind() {
        ErrorKind::NotFound => io::Error::new(ErrorKind::NotFound, "not found"),
        ErrorKind::NotADirectory => io::Error::new(ErrorKind::NotADirectory, "not a directory"),
        ErrorKind::IsADirectory => io::Error::other("is a directory"),
        ErrorKind::DirectoryNotEmpty => io::Error::other("directory not empty"),
        ErrorKind::PermissionDenied => {
            io::Error::new(ErrorKind::PermissionDenied, "Permission denied")
        }
        ErrorKind::AlreadyExists => io::Error::new(ErrorKind::AlreadyExists, "File exists"),
        _ => e,
    };
    bashkit::Error::Io(conventional)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_only_errors_get_their_kind_back_and_survive_the_round_trip() {
        // bashkit's own "is a directory" shape (fs_errors::is_a_directory).
        let e = io_from_bashkit(io::Error::other("is a directory").into());
        assert_eq!(e.kind(), ErrorKind::IsADirectory);
        let back = io_from_bashkit(bashkit_from_io(e));
        assert_eq!(back.kind(), ErrorKind::IsADirectory);
        let nf = io_from_bashkit(bashkit_from_io(io::Error::from(ErrorKind::NotFound)));
        assert_eq!(nf.kind(), ErrorKind::NotFound);
    }
}
