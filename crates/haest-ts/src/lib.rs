//! TypeScript to JavaScript, in pure Rust, for haest's embedded QuickJS.
//!
//! QuickJS runs JavaScript only. haest runs TypeScript in two places: the
//! sandbox shell's `js file.ts` command (`haest-code`) and `.ts` mod files
//! (`haest-mods`). Both go through this crate's one function,
//! [`strip_types`], so a script and a mod see the same TypeScript rules. No
//! node, bun or deno is needed at runtime.
//!
//! The stripper is swc's (`swc_ts_fast_strip`, the one Node uses for
//! `--experimental-strip-types`). Plain type annotations are erased in place,
//! so line numbers -- the `(line N)` of an error -- stay the script's own.
//! Syntax that has to become code (an `enum`, a `namespace`, a constructor
//! parameter property) is transformed instead, and then lines can move.
//! Nothing is type-checked: a type error is not an error here.
//!
//! # When to use it directly
//!
//! Through `haest-code` and `haest-mods` you never call it yourself. Take this
//! crate when you embed a JavaScript engine of your own and want TypeScript in
//! it with the same rules and the same limits ([`MAX_SOURCE_BYTES`]).
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), haest_ts::StripError> {
//! let ts = "const n: number = 1\nconsole.log(n as number)";
//! let js = haest_ts::strip_types(ts, "main.ts")?;
//! assert!(!js.contains("number"));
//! // Line 2 is still line 2.
//! assert!(js.lines().nth(1).is_some_and(|line| line.contains("console.log(n")));
//!
//! // An enum has no JavaScript form of its own: it becomes code.
//! let js = haest_ts::strip_types("enum Color { Red, Green }", "colors.ts")?;
//! assert!(!js.contains("enum "));
//!
//! // A syntax error is an Err that names the problem.
//! let err = haest_ts::strip_types("const = ;", "broken.ts").expect_err("not TypeScript");
//! assert!(matches!(err, haest_ts::StripError::Syntax { .. }));
//! # Ok(())
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use serde_json::json;
use swc_common::{GLOBALS, Globals, SourceMap, errors::Handler, sync::Lrc};
use swc_ts_fast_strip::{ErrorCode, Options, operate};

/// Collects what swc reports, so the first diagnostic can name the problem.
#[derive(Clone, Default)]
struct Diagnostics(Arc<Mutex<Vec<u8>>>);

impl Write for Diagnostics {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Diagnostics {
    fn first_line(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned();
        text.lines()
            .map(|l| l.trim().trim_start_matches(['x', '×', ' ']).trim())
            .find(|l| !l.is_empty())
            .map(String::from)
    }
}

/// The longest source [`strip_types`] accepts, in bytes (128 KiB); a longer
/// one is an `Err`, never stripped.
///
/// The cap is about the stack, not time. swc recurses once per level of
/// nesting with no limit of its own, and one byte can open a level (`(`), so
/// the stack it runs on is sized by the source, never the caller's: a tokio
/// worker's 2 MiB held about 600 unclosed `(` in a release build, and a stack
/// overflow aborts the whole process.
pub const MAX_SOURCE_BYTES: usize = 128 * 1024;

/// Stack per byte of source, over a measured worst of one unclosed `(` a byte:
/// 3.4 KB in a release build, 20 KB in a debug one (swc_ecma_parser 47). Only
/// what a parse touches is committed; the rest is address space.
const STACK_PER_BYTE: usize = if cfg!(debug_assertions) { 24 } else { 4 } << 10;
const STACK_BASE: usize = 8 << 20;

/// Why [`strip_types`] gave no JavaScript. More variants may come in a minor
/// release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StripError {
    /// The source is longer than [`MAX_SOURCE_BYTES`]; it was not parsed.
    #[error(
        "{} KB of TypeScript is past the {} KB haest strips; write it as JavaScript, or split it",
        bytes >> 10,
        MAX_SOURCE_BYTES >> 10
    )]
    TooLong {
        /// The source's length.
        bytes: usize,
    },
    /// It is not valid TypeScript.
    #[error("{message}")]
    Syntax {
        /// The first diagnostic swc reports, for a human or a model.
        message: String,
    },
    /// The stripper's thread could not start.
    #[error("the TypeScript stripper could not start")]
    Spawn(#[source] io::Error),
    /// swc panicked on this source.
    #[error("the TypeScript stripper failed on this source")]
    Panicked,
}

/// Turns TypeScript `source` into JavaScript that QuickJS can run.
///
/// Type annotations, `as` casts, interfaces and type-only imports are
/// replaced by spaces, so every line and column keeps its place. When the
/// source has syntax that must become code (an `enum`, a `namespace`, a
/// constructor parameter property), the whole file is transformed instead and
/// its lines can move. Types are not checked. The source is read as an ES
/// module.
///
/// `filename` is used for two things: a name ending in `x` (`app.tsx`) is
/// parsed as TSX, and it names the file in the stripper's own diagnostics.
/// Nothing is read from disk. JSX in a `.tsx` file is left as JSX, which
/// QuickJS cannot run.
///
/// The work runs on a new thread with a stack sized for this source, and the
/// call blocks until it is done: from async code, call it through
/// `tokio::task::spawn_blocking` (or the like), so it does not hold a worker.
///
/// # Errors
///
/// A [`StripError`]: the source is longer than [`MAX_SOURCE_BYTES`]
/// ([`TooLong`](StripError::TooLong)); it is not valid TypeScript
/// ([`Syntax`](StripError::Syntax), with swc's first diagnostic); the
/// stripper's thread could not start ([`Spawn`](StripError::Spawn)) or
/// panicked on this source ([`Panicked`](StripError::Panicked)).
///
/// A panic inside swc comes back as an `Err`; this function does not panic.
///
/// # Examples
///
/// ```
/// let ts = "export const greet = (who: string): string => `hi ${who}`";
/// let js = haest_ts::strip_types(ts, "greet.ts")?;
/// assert!(!js.contains("string"));
/// // The types became spaces: every column is where it was.
/// assert_eq!(js.find("=>"), ts.find("=>"));
/// # Ok::<(), haest_ts::StripError>(())
/// ```
pub fn strip_types(source: &str, filename: &str) -> Result<String, StripError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(StripError::TooLong {
            bytes: source.len(),
        });
    }
    let (owned, name) = (source.to_string(), filename.to_string());
    let stripped = std::thread::Builder::new()
        .name("haest-ts".into())
        .stack_size(STACK_BASE + source.len() * STACK_PER_BYTE)
        .spawn(move || strip_here(&owned, &name))
        .map_err(StripError::Spawn)?
        .join()
        .map_err(|_| StripError::Panicked)?;
    stripped.map_err(|message| StripError::Syntax { message })
}

fn strip_here(source: &str, filename: &str) -> Result<String, String> {
    match strip(source, filename, "strip-only") {
        Err((ErrorCode::UnsupportedSyntax, _)) => {
            strip(source, filename, "transform").map_err(|(_, why)| why)
        }
        other => other.map_err(|(_, why)| why),
    }
}

fn strip(source: &str, filename: &str, mode: &str) -> Result<String, (ErrorCode, String)> {
    let options: Options = serde_json::from_value(json!({
        "module": true,
        "filename": filename,
        "mode": mode,
        "parser": {"tsx": filename.ends_with('x'), "decorators": true},
    }))
    .map_err(|e| (ErrorCode::Unknown, e.to_string()))?;
    let cm: Lrc<SourceMap> = Lrc::default();
    let diagnostics = Diagnostics::default();
    let handler = Handler::with_emitter_writer(Box::new(diagnostics.clone()), Some(cm.clone()));
    GLOBALS
        .set(&Globals::new(), || {
            operate(&cm, &handler, source.to_string(), options)
        })
        .map(|out| out.code)
        .map_err(|e| (e.code, diagnostics.first_line().unwrap_or(e.message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_go_and_lines_stay() {
        let js = strip_types("const n: number = 1\nlet s: string\nnull.x()", "a.ts").unwrap();
        assert!(!js.contains(": number"));
        assert_eq!(js.lines().count(), 3);
        assert!(js.lines().nth(2).unwrap().contains("null.x()"));
    }

    #[test]
    fn an_enum_becomes_code() {
        let js = strip_types("enum E { A, B }\nconsole.log(E.B)", "a.ts").unwrap();
        assert!(!js.contains("enum "));
    }

    /// `strip_types` on a fresh thread with a tokio worker's stack (2 MiB),
    /// where the sandbox's `js` calls it. An overflow aborts the test binary.
    fn on_a_worker_stack(source: String, filename: &'static str) -> Result<String, StripError> {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || strip_types(&source, filename))
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn deep_nesting_is_stripped_or_refused_never_a_crash() {
        // A level per byte, each about 3 KB of swc's stack (20 KB in a debug build).
        const K: usize = 5_000;
        let deep = format!("let x: number = {}1{}", "(".repeat(K), ")".repeat(K));
        let js = on_a_worker_stack(deep, "a.ts").unwrap();
        assert!(!js.contains(": number"));
        let with_enum = format!(
            "enum E {{ A }}\nlet x = {}E.A{}",
            "[".repeat(K),
            "]".repeat(K)
        );
        assert!(
            !on_a_worker_stack(with_enum, "a.ts")
                .unwrap()
                .contains("enum ")
        );
        assert!(on_a_worker_stack("(".repeat(K), "a.ts").is_err());
        assert!(on_a_worker_stack("<a>".repeat(K), "a.tsx").is_err());
        // Past what a stack is sized for: refused before swc sees it.
        let long = on_a_worker_stack("(".repeat(200_000), "a.ts");
        assert!(matches!(long, Err(StripError::TooLong { bytes: 200_000 })));
    }

    #[test]
    fn a_syntax_error_is_an_error() {
        let err = strip_types("const = ;", "a.ts").unwrap_err();
        assert!(matches!(err, StripError::Syntax { .. }), "{err:?}");
    }
}
