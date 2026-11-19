//! Where a test's project is: a temp dir, or a directory on a disk that
//! folds names (exFAT, FAT, vfat), and the renames by case run on either.

use std::{fs, path::PathBuf, process::Command};

use haest_workspace::SandboxWorkspace;

use super::project::{apply_all, changes, contents, nothing_lost, pairs, paths, reads_as_one, run};
use crate::common::sandbox;

/// Names a directory on a disk that reads two names of a file as one and
/// lists the one it holds (exFAT, FAT on macOS; vfat on Linux). Unset, the
/// tests on that disk pass without running.
pub(crate) const FOLDING_DIR: &str = "HAEST_TEST_FOLDING_DIR";

/// A shell command that drops every name that disk has cached (on macOS,
/// detach the disk image and attach it again): macOS answers `realpath`
/// there with the name a file was first looked up by since then. Run after
/// the human's files are made and before the sandbox opens; unset, nothing
/// is dropped.
pub(crate) const FOLDING_REMOUNT: &str = "HAEST_TEST_FOLDING_REMOUNT";

/// A project directory, made for one test and removed after it.
pub(crate) struct Disk {
    _t: tempfile::TempDir,
    /// The directory by its real path, as the sandbox mounts it.
    pub(crate) dir: PathBuf,
    /// Run before the sandbox opens ([`FOLDING_REMOUNT`]).
    remount: Option<String>,
}

impl Disk {
    /// A new directory on the temp disk.
    pub(crate) fn temp() -> Self {
        let (t, dir) = crate::common::tempdir();
        Self {
            _t: t,
            dir,
            remount: None,
        }
    }

    /// A new directory on the disk [`FOLDING_DIR`] is on; None where unset.
    pub(crate) fn folding() -> Option<Self> {
        let base = std::env::var_os(FOLDING_DIR)?;
        let t = tempfile::tempdir_in(base).unwrap();
        let dir = fs::canonicalize(t.path()).unwrap();
        let remount = std::env::var(FOLDING_REMOUNT).ok();
        Some(Self {
            _t: t,
            dir,
            remount,
        })
    }

    /// The human's `files` (path, text), with the directories they are in.
    pub(crate) fn make(&self, files: &[(&str, &str)]) {
        for (path, text) in files {
            let real = self.dir.join(path);
            fs::create_dir_all(real.parent().unwrap()).unwrap();
            fs::write(real, text).unwrap();
        }
    }

    /// The default sandbox over the project, once the disk has dropped the
    /// names it cached.
    pub(crate) async fn sandbox(&self) -> SandboxWorkspace {
        if let Some(command) = &self.remount {
            let done = Command::new("sh").arg("-c").arg(command).status().unwrap();
            assert!(done.success(), "{FOLDING_REMOUNT}: {done}");
        }
        sandbox(&self.dir).await
    }
}

/// The human's files, what the model does to them (a rename by case, most
/// of all), and what the disk holds once everything is applied where it
/// tells the names apart. Where the disk reads the two names `names` as
/// one, every change that would land on the other name, or take what the
/// sandbox shows under it, is held as a conflict: the disk keeps the
/// human's files as they were, and nothing the sandbox shows is lost.
pub(crate) struct Renamed {
    pub(crate) files: &'static [(&'static str, &'static str)],
    pub(crate) command: &'static str,
    pub(crate) want: &'static [(&'static str, &'static str)],
    /// A name among `files`, and the other name the command gives it; None
    /// where the command renames nothing by case.
    pub(crate) names: Option<(&'static str, &'static str)>,
}

impl Renamed {
    /// Run it on `disk`.
    pub(crate) async fn check(&self, disk: &Disk) {
        disk.make(self.files);
        let one = self
            .names
            .is_some_and(|(name, other)| reads_as_one(&disk.dir, name, other));
        let ws = disk.sandbox().await;
        run(&ws, &disk.dir, self.command).await;
        if !one {
            apply_all(&ws).await;
            assert_eq!(contents(&disk.dir), pairs(self.want), "{}", self.command);
            return;
        }
        let report = changes(&ws).apply().await;
        nothing_lost(&ws, &disk.dir, &report).await;
        assert!(report.applied.is_empty(), "{}: {report:?}", self.command);
        let kept = contents(&disk.dir);
        assert_eq!(kept, pairs(self.files), "{}: {report:?}", self.command);
    }
}

/// A rename by case, a copy beside it and the original removed.
pub(crate) const RENAME_AND_COPY: Renamed = Renamed {
    files: &[("guide.md", "guide\n"), ("readme.md", "kept\n")],
    command: "mv readme.md README.md && cp guide.md GUIDE.md && rm guide.md",
    want: &[("GUIDE.md", "guide\n"), ("README.md", "kept\n")],
    names: Some(("readme.md", "README.md")),
};

/// A rename by case to lower case.
pub(crate) const RENAME_TO_LOWER: Renamed = Renamed {
    files: &[("README.md", "the human readme\n")],
    command: "mv README.md readme.md",
    want: &[("readme.md", "the human readme\n")],
    names: Some(("README.md", "readme.md")),
};

/// A rename by case to upper case, then an edit under the new name.
pub(crate) const RENAME_THEN_EDIT: Renamed = Renamed {
    files: &[("notes.md", "notes\n")],
    command: "mv notes.md NOTES.md && echo more >> NOTES.md",
    want: &[("NOTES.md", "notes\nmore\n")],
    names: Some(("notes.md", "NOTES.md")),
};

/// A rename by case to lower case, then an edit under the new name.
pub(crate) const RENAME_TO_LOWER_THEN_EDIT: Renamed = Renamed {
    files: &[("Makefile", "all:\n")],
    command: "mv Makefile makefile && echo '# edited' >> makefile",
    want: &[("makefile", "all:\n# edited\n")],
    names: Some(("Makefile", "makefile")),
};

/// A directory renamed by case.
pub(crate) const DIRECTORY: Renamed = Renamed {
    files: &[("lib/a.rs", "a\n"), ("lib/b.rs", "b\n")],
    command: "mv lib Lib",
    want: &[("Lib/a.rs", "a\n"), ("Lib/b.rs", "b\n")],
    names: Some(("lib", "Lib")),
};

/// A rename by case of a name with no ASCII letter: Greek has case too,
/// on a disk that folds more than ASCII.
pub(crate) const RENAME_WITH_NO_ASCII_LETTER: Renamed = Renamed {
    files: &[("\u{391}\u{392}\u{393}", "the human's\n")],
    command: "mv \u{391}\u{392}\u{393} \u{3b1}\u{3b2}\u{3b3}",
    want: &[("\u{3b1}\u{3b2}\u{3b3}", "the human's\n")],
    names: Some(("\u{391}\u{392}\u{393}", "\u{3b1}\u{3b2}\u{3b3}")),
};

/// A directory and files named by number alone, which have no case: one
/// edited, one added, one removed. It applies on any disk.
pub(crate) const NUMBERED: Renamed = Renamed {
    files: &[("2024/01", "one\n"), ("2024/02", "two\n")],
    command: "echo more >> 2024/01 && echo three > 2024/03 && rm 2024/02",
    want: &[("2024/01", "one\nmore\n"), ("2024/03", "three\n")],
    names: None,
};

/// The model copies `README.md` to `readme.md` and edits the copy. Where
/// the disk reads both names as one, the copy would land on the human's
/// file: it is a conflict, skipped and still listed, and the disk keeps the
/// human's file as it was. Where it tells them apart, both are applied.
pub(crate) async fn copy_by_case_then_edit(disk: &Disk) {
    const HUMAN: &str = "the human readme\n";
    disk.make(&[("README.md", HUMAN)]);
    let one = reads_as_one(&disk.dir, "README.md", "readme.md");
    let ws = disk.sandbox().await;
    run(
        &ws,
        &disk.dir,
        "cp README.md readme.md && echo more >> readme.md",
    )
    .await;
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &disk.dir, &report).await;
    let left = changes(&ws).list().await;
    if one {
        assert!(report.applied.is_empty(), "{report:?}");
        assert_eq!(paths(&report.skipped), ["/readme.md"], "{report:?}");
        assert_eq!(contents(&disk.dir), pairs(&[("README.md", HUMAN)]));
        assert_eq!(paths(&left), ["/readme.md"]);
    } else {
        assert_eq!(paths(&report.applied), ["/readme.md"], "{report:?}");
        let want = [
            ("README.md", HUMAN),
            ("readme.md", "the human readme\nmore\n"),
        ];
        assert_eq!(contents(&disk.dir), pairs(&want));
        assert!(left.is_empty(), "{left:?}");
    }
}
