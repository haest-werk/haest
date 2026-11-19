//! What the system says of a disk, and on Linux of a directory, before any
//! name in it is asked: the file system's kind (`statfs`), and a directory's
//! case-folding flag (`chattr +F`).

use std::path::Path;

use super::folding::Disk;

/// `dir` as a NUL-terminated string for a system call; None where it holds
/// a NUL.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn c_path(dir: &Path) -> Option<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()
}

/// The disk `dir` is on, by the kind `statfs` names: APFS and HFS+ answer
/// `realpath` with the name a directory holds; exFAT and FAT read two
/// spellings as one everywhere.
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "statfs(2) has no std wrapper")]
pub(super) fn disk(dir: &Path) -> Disk {
    let Some(c) = c_path(dir) else {
        return Disk::Unknown;
    };
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c` is a NUL-terminated path that outlives the call, and
    // statfs writes one `struct statfs` at the pointer, which has room for
    // it.
    if unsafe { libc::statfs(c.as_ptr(), fs.as_mut_ptr()) } != 0 {
        return Disk::Unknown;
    }
    // SAFETY: statfs succeeded, so it filled the struct in.
    let fs = unsafe { fs.assume_init() };
    let kind: Vec<u8> = fs
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| c.cast_unsigned())
        .collect();
    match kind.as_slice() {
        b"apfs" | b"hfs" => Disk::Stored,
        b"exfat" | b"msdos" => Disk::Folds,
        _ => Disk::Unknown,
    }
}

/// The disk `dir` is on, by the magic number `statfs` gives (linux/magic.h):
/// vfat, msdos and exFAT read two spellings as one everywhere; ext2-4,
/// f2fs, tmpfs and btrfs only in a directory flagged so, and overlayfs as
/// its layers do. The rest (XFS has a whole-disk ASCII folding no flag
/// shows; ZFS, NFS, SMB and FUSE fold as their server or dataset does) is
/// told by a name in each directory.
#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "statfs(2) has no std wrapper")]
pub(super) fn disk(dir: &Path) -> Disk {
    const MSDOS: u32 = 0x4d44;
    const EXFAT: u32 = 0x2011_bab0;
    const EXT: u32 = 0xef53;
    const F2FS: u32 = 0xf2f5_2010;
    const TMPFS: u32 = 0x0102_1994;
    const BTRFS: u32 = 0x9123_683e;
    const OVERLAYFS: u32 = 0x794c_7630;
    let Some(c) = c_path(dir) else {
        return Disk::Unknown;
    };
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c` is a NUL-terminated path that outlives the call, and
    // statfs writes one `struct statfs` at the pointer, which has room for
    // it.
    if unsafe { libc::statfs(c.as_ptr(), fs.as_mut_ptr()) } != 0 {
        return Disk::Unknown;
    }
    // SAFETY: statfs succeeded, so it filled the struct in.
    let fs = unsafe { fs.assume_init() };
    match u32::try_from(fs.f_type) {
        Ok(MSDOS | EXFAT) => Disk::Folds,
        Ok(EXT | F2FS | TMPFS | BTRFS | OVERLAYFS) => Disk::Flagged,
        _ => Disk::Unknown,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn disk(_: &Path) -> Disk {
    Disk::Unknown
}

/// Whether the directory `dir` reads two spellings of a name as one, by its
/// own flag (`FS_CASEFOLD_FL`, set with `chattr +F`); None where its flags
/// cannot be read.
///
/// Opened as a directory, without waiting: a FIFO put in its place after
/// the lookup's `lstat` fails at once, where opening it to read would wait
/// for a writer, and hold the lookup and a blocking thread while none came.
#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "ioctl(2) has no std wrapper")]
pub(super) fn flag(dir: &Path) -> Option<bool> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    /// `FS_CASEFOLD_FL` (linux/fs.h).
    const CASEFOLD: libc::c_int = 0x4000_0000;
    /// Room for what the request names (`_IOR('f', 1, long)`: a long), though
    /// the kernel's own handlers write an int: one served by something else
    /// (FUSE before Linux 5.13) may write all of it.
    type Flags = [libc::c_int; 2];
    const _: () = assert!(size_of::<Flags>() >= size_of::<libc::c_long>());
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK)
        .open(dir)
        .ok()?;
    let mut flags: Flags = [0; 2];
    // SAFETY: the descriptor stays open for the call, and FS_IOC_GETFLAGS
    // writes the file's flags at the pointer: an int from the kernel's own
    // handlers, at most the long the request encodes from any other, and
    // `flags` has room for either. The int is its first element.
    let done = unsafe { libc::ioctl(dir.as_raw_fd(), libc::FS_IOC_GETFLAGS, flags.as_mut_ptr()) };
    (done == 0).then_some(flags[0] & CASEFOLD != 0)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn flag(_: &Path) -> Option<bool> {
    None
}
