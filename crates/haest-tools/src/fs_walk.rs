//! Glob over any [`Fs`]: the walk behind the Glob tool ([`glob_fs`]).
//!
//! globset only MATCHES here (pure string work); the directory walk goes
//! through the workspace, so the sandbox's in-memory files are found exactly
//! like real ones.
//!
//! The pattern language is the TS haest's (Bun's glob): `*` stops at `/`, `**`
//! crosses directories, `?`, `[...]`, `{a,b}`, dot files match, case matters, and
//! a leading `!` negates the whole pattern.
//!
//! ```
//! use std::path::Path;
//!
//! use haest_core::fs::MemFs;
//! use haest_tools::fs_walk::glob_fs;
//! use tokio_util::sync::CancellationToken;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let fs = MemFs::with_files([
//!     ("/p/src/a.rs", ""),
//!     ("/p/src/deep/b.rs", ""),
//!     ("/p/node_modules/c.rs", ""),
//! ]);
//! let found = glob_fs(&fs, Path::new("/p"), "**/*.rs", &CancellationToken::new())
//!     .await
//!     .unwrap();
//! let mut paths: Vec<_> = found.hits.iter().map(|h| h.path.display().to_string()).collect();
//! paths.sort();
//! // node_modules is never entered.
//! assert_eq!(paths, ["/p/src/a.rs", "/p/src/deep/b.rs"]);
//! assert!(!found.capped);
//! # }
//! ```

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use globset::{GlobBuilder, GlobMatcher};
use haest_core::fs::{FileKind, Fs, normalize};
use tokio_util::sync::CancellationToken;

const SKIP_DIRS: [&str; 2] = ["node_modules", ".git"];
/// A walk that visits this many entries stops and says so, never hangs a turn.
pub const WALK_LIMIT: usize = 100_000;

/// One file a glob matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobHit {
    /// Its absolute path.
    pub path: PathBuf,
    /// When it last changed; the Unix epoch when the `Fs` does not say.
    pub mtime: SystemTime,
}

/// What a walk found ([`glob_fs`]).
#[derive(Debug, Default)]
pub struct GlobResult {
    /// The matching files, in walk order.
    pub hits: Vec<GlobHit>,
    /// True when [`WALK_LIMIT`] stopped the walk before the tree was exhausted.
    pub capped: bool,
}

const GLOB_CHARS: &[char] = &['*', '?', '[', ']', '{', '}', '!'];

/// The leading pattern segments with no glob syntax: the walk starts there.
fn static_prefix(pattern: &str) -> (String, String) {
    let mut parts: Vec<&str> = pattern.split('/').collect();
    let mut fixed = Vec::new();
    while parts.len() > 1 && !parts[0].contains(GLOB_CHARS) {
        fixed.push(parts.remove(0));
    }
    (fixed.join("/"), parts.join("/"))
}

/// One compiled pattern, matched against paths relative to the walk's start.
///
/// ```
/// use haest_tools::fs_walk::Matcher;
///
/// let m = Matcher::new("src/*.rs").unwrap();
/// assert!(m.is_match("src/a.rs"));
/// assert!(!m.is_match("src/deep/a.rs"));
/// assert!(Matcher::new("!*.md").unwrap().is_match("a.rs"));
/// ```
pub struct Matcher {
    glob: GlobMatcher,
    negate: bool,
}

impl Matcher {
    /// `pattern`, compiled; a leading `!` negates it.
    ///
    /// # Errors
    ///
    /// A pattern globset cannot parse (an unclosed `[`), with a message for
    /// the model.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let (negate, body) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let glob = GlobBuilder::new(body)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|e| format!("invalid glob pattern \"{pattern}\": {e}"))?;
        Ok(Self {
            glob: glob.compile_matcher(),
            negate,
        })
    }

    /// Whether `rel`, a `/`-separated path relative to the walk's start, matches.
    pub fn is_match(&self, rel: &str) -> bool {
        self.glob.is_match(rel) != self.negate
    }
}

struct Dir {
    abs: PathBuf,
    rel: String,
}

struct Walk<'a> {
    fs: &'a dyn Fs,
    matcher: Matcher,
    hits: Vec<GlobHit>,
    stack: Vec<Dir>,
    visited: usize,
}

impl Walk<'_> {
    /// Visit one directory's entries: queue subdirectories, collect matching files.
    /// False = walk limit hit.
    async fn visit(&mut self, dir: Dir) -> bool {
        let entries = self.fs.read_dir(&dir.abs).await.unwrap_or_default();
        for entry in entries {
            self.visited += 1;
            if self.visited > WALK_LIMIT {
                return false;
            }
            if SKIP_DIRS.contains(&entry.name.as_str()) {
                continue;
            }
            let child = Dir {
                abs: dir.abs.join(&entry.name),
                rel: if dir.rel.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", dir.rel, entry.name)
                },
            };
            // A symlink is never descended (its kind is Symlink, not Dir), so a
            // link loop cannot spin the walk.
            if entry.kind == FileKind::Dir {
                self.stack.push(child);
            } else if self.matcher.is_match(&child.rel)
                && let Ok(st) = self.fs.stat(&child.abs).await
            {
                self.hits.push(GlobHit {
                    path: child.abs,
                    mtime: st.modified.unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
        true
    }
}

/// Every file under `base` matching `pattern`, in walk order.
///
/// The leading segments with no glob syntax pick where the walk starts
/// (`src/**/*.ts` walks `base/src` only; an absolute `/src/*.ts` walks
/// `/src`), the rest is matched against the path below it. A directory that
/// cannot be read is skipped. A symlink is matched like a file and never
/// descended, so a link loop cannot spin the walk; node_modules and .git are
/// never entered. The walk stops after [`WALK_LIMIT`] entries
/// ([`GlobResult::capped`]), and when `cancel` fires, with what it found so far.
///
/// # Errors
///
/// An invalid pattern, with a message for the model.
pub async fn glob_fs(
    fs: &dyn Fs,
    base: &Path,
    pattern: &str,
    cancel: &CancellationToken,
) -> Result<GlobResult, String> {
    let (dir, rest) = static_prefix(pattern);
    let start = if dir.is_empty() {
        base.to_path_buf()
    } else {
        // An absolute pattern ("/src/*.ts") starts at its own root, not under base.
        normalize(&base.join(&dir))
    };
    let mut w = Walk {
        fs,
        matcher: Matcher::new(&rest)?,
        hits: Vec::new(),
        stack: vec![Dir {
            abs: start,
            rel: String::new(),
        }],
        visited: 0,
    };
    let mut capped = false;
    while !capped
        && !cancel.is_cancelled()
        && let Some(next) = w.stack.pop()
    {
        capped = !w.visit(next).await;
    }
    Ok(GlobResult {
        hits: w.hits,
        capped,
    })
}

#[cfg(test)]
mod tests;
