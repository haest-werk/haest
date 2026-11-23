//! What [`ModFiles`](super::ModFiles) knows of each mod file, and one look
//! at the disk that acts on what changed ([`Files::look`]): when the
//! conversation starts, and at each step boundary while the files reload.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
};

use haest_code::VmCaps;
use haest_core::{error::report, fs::FsRef};
use haest_mods::{ApprovalDecision, ApprovalRequest, ModEnv, ModRef, Mods};
use haest_settings::ModReload;
use tokio_util::sync::CancellationToken;

use super::{super::JsMod, mod_files};

/// When a look happens.
#[derive(Clone, Copy)]
pub(super) enum Look<'a> {
    /// The conversation starts: every file there loads, without asking.
    Start,
    /// A step boundary: a change loads as `mod_reload` says; `cancel` ends
    /// the human's ask.
    Step(&'a CancellationToken),
}

/// A file's place in the run order: its directory's place in the mod
/// directories, then its name.
type Key = (usize, String);

/// One mod file, as last seen.
#[derive(Default)]
struct File {
    /// The version that runs: its content, and the mod loaded from it.
    running: Option<(String, ModRef)>,
    /// What the disk held when last acted on (loaded, failed, denied): its
    /// content, None once gone. A look acts only on a disk that differs.
    settled: Option<String>,
    /// What the human denied (None: the file's deletion), never asked again.
    denied: HashSet<Option<String>>,
    /// Its last read failed, and that was reported: not again until a read
    /// works.
    unreadable: bool,
}

impl File {
    fn running_content(&self) -> Option<&String> {
        self.running.as_ref().map(|(content, _)| content)
    }

    /// Whether to keep it: it runs, the disk has it, or it holds a memory.
    fn kept(&self) -> bool {
        self.running.is_some()
            || self.settled.is_some()
            || self.unreadable
            || !self.denied.is_empty()
    }
}

/// What a look read of one file.
enum OnDisk {
    Content(String),
    Unreadable(String),
}

/// Every mod file of a conversation, and how to load one.
pub(super) struct Files {
    dirs: Vec<PathBuf>,
    caps: VmCaps,
    fs: FsRef,
    env: ModEnv,
    reload: ModReload,
    known: BTreeMap<Key, File>,
}

impl Files {
    pub(super) fn new(
        dirs: Vec<PathBuf>,
        caps: VmCaps,
        fs: FsRef,
        env: ModEnv,
        reload: ModReload,
    ) -> Self {
        Self {
            dirs,
            caps,
            fs,
            env,
            reload,
            known: BTreeMap::new(),
        }
    }

    /// Whether the files reload at each step.
    pub(super) fn reloads(&self) -> bool {
        self.reload != ModReload::Off
    }

    /// The versions that run, in order, as one [`Mods`].
    pub(super) fn mods(&self) -> Mods {
        let running = self.known.values().filter_map(|f| f.running.as_ref());
        Mods::new(self.env.clone(), running.map(|(_, m)| m.clone()))
    }

    fn path(&self, (dir, name): &Key) -> PathBuf {
        self.dirs[*dir].join(name)
    }

    /// Every mod file in the mod directories now, read.
    async fn read_disk(&self) -> BTreeMap<Key, OnDisk> {
        let mut disk = BTreeMap::new();
        for (i, dir) in self.dirs.iter().enumerate() {
            for path in mod_files(&*self.fs, dir).await {
                let Some(name) = path.file_name() else {
                    continue;
                };
                let read = match self.fs.read_to_string(&path).await {
                    Ok(content) => OnDisk::Content(content),
                    Err(e) => OnDisk::Unreadable(e.to_string()),
                };
                disk.insert((i, name.to_string_lossy().into_owned()), read);
            }
        }
        disk
    }

    /// Read every file and act on what changed since the last look: a new
    /// or changed file loads (in `ask`, on the human's Allow), a deleted one
    /// unloads (in `ask`, on their Allow too), one that fails to load keeps
    /// the version before and is reported once. Whether what runs changed.
    pub(super) async fn look(&mut self, look: Look<'_>) -> bool {
        let mut disk = self.read_disk().await;
        let keys: BTreeSet<Key> = self.known.keys().chain(disk.keys()).cloned().collect();
        let mut changed = false;
        for key in keys {
            let path = self.path(&key);
            let now = match disk.remove(&key) {
                Some(OnDisk::Unreadable(why)) => {
                    self.unreadable(key, &path, &why, look);
                    continue;
                }
                Some(OnDisk::Content(content)) => Some(content),
                None => None,
            };
            let mut file = self.known.remove(&key).unwrap_or_default();
            file.unreadable = false;
            changed |= self.settle(&mut file, &path, now, look).await;
            if file.kept() {
                self.known.insert(key, file);
            }
        }
        changed
    }

    /// The file at `path` could not be read: what runs stays, and the human
    /// hears it once.
    fn unreadable(&mut self, key: Key, path: &Path, why: &str, look: Look<'_>) {
        let file = self.known.entry(key).or_default();
        if file.unreadable {
            return;
        }
        file.unreadable = true;
        let path = path.display();
        self.env.error(&match (look, file.running.is_some()) {
            (Look::Step(_), true) => {
                format!("Mod {path} could not be read and the version before runs: {why}")
            }
            _ => format!("Mod {path} failed to load and was skipped: {why}"),
        });
    }

    /// Act on `now`, what the disk holds of `file`. Whether what runs changed.
    async fn settle(
        &self,
        file: &mut File,
        path: &Path,
        now: Option<String>,
        look: Look<'_>,
    ) -> bool {
        if file.settled == now {
            return false;
        }
        if file.running_content() == now.as_ref() || file.denied.contains(&now) {
            file.settled = now;
            return false;
        }
        if let Look::Step(cancel) = look
            && self.reload == ModReload::Ask
        {
            let asked = self.env.ask(ask(path, now.as_ref(), file), cancel).await;
            if cancel.is_cancelled() {
                // Not answered: asked again at the next step.
                return false;
            }
            if asked == ApprovalDecision::Deny {
                self.env.info(&kept(path, now.as_ref(), file));
                file.denied.insert(now.clone());
                file.settled = now;
                return false;
            }
        }
        let shown = path.display();
        let Some(content) = now else {
            file.running = None;
            file.settled = None;
            self.env
                .info(&format!("Mod {shown} was deleted and is unloaded."));
            return true;
        };
        file.settled = Some(content.clone());
        let loaded = JsMod::load_with(path, content.clone(), &self.env, self.caps).await;
        match loaded {
            Ok(m) => {
                let was = file.running.replace((content, ModRef::new(m)));
                if matches!(look, Look::Step(_)) {
                    let done = if was.is_some() { "reloaded" } else { "loaded" };
                    self.env.info(&format!("Mod {shown} {done}."));
                }
                true
            }
            Err(e) => {
                let why = report(&e);
                self.env.error(&if file.running.is_some() {
                    format!("Mod {shown} failed to reload and the version before runs: {why}")
                } else {
                    format!("Mod {shown} failed to load and was skipped: {why}")
                });
                false
            }
        }
    }
}

/// The human's ask about a change to `file`: the new content whole (it is
/// what runs), or its deletion.
fn ask(path: &Path, now: Option<&String>, file: &File) -> ApprovalRequest {
    let name = path.display().to_string();
    match now {
        Some(content) => {
            let what = if file.running.is_some() {
                "haest wants to load the new version of this mod file: it runs with haest's trust from the next model call"
            } else {
                "haest wants to load this new mod file: it runs with haest's trust from the next model call"
            };
            ApprovalRequest::new(name, what)
                .with_input_preview(content.as_str())
                .with_key(content.as_str())
        }
        None => ApprovalRequest::new(
            name,
            "haest wants to unload this mod file: it was deleted, and what it does stops at the next model call",
        ),
    }
}

/// What the human's Deny kept, in one line.
fn kept(path: &Path, now: Option<&String>, file: &File) -> String {
    let path = path.display();
    match (now, file.running.is_some()) {
        (None, _) => format!("Mod {path} was deleted but still runs."),
        (Some(_), true) => format!("Mod {path} changed; the version before runs."),
        (Some(_), false) => format!("Mod {path} was not loaded."),
    }
}
