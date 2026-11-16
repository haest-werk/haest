//! TypeScript to JavaScript for kern's embedded QuickJS (the sandbox `js`
//! command, `.ts` mod files): the ONE stripper both use. QuickJS runs
//! JavaScript only; this is swc's type stripper (the one Node uses for
//! `--experimental-strip-types`). Plain type annotations are erased in place, so
//! line numbers -- the `(line N)` of an error -- stay the script's own; syntax
//! that has to become code (an `enum`, a `namespace`) is transformed instead.

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

/// The longest source stripped. swc recurses once per level of nesting with no
/// limit of its own, and one byte can open a level (`(`), so the stack it runs
/// on is sized by the source (below), never the caller's: a tokio worker's 2
/// MiB held about 600 unclosed `(` in a release build, and an overflow aborts
/// the whole process.
pub const MAX_SOURCE_BYTES: usize = 128 * 1024;

/// Stack per byte of source, over a measured worst of one unclosed `(` a byte:
/// 3.4 KB in a release build, 20 KB in a debug one (swc_ecma_parser 47). Only
/// what a parse touches is committed; the rest is address space.
const STACK_PER_BYTE: usize = if cfg!(debug_assertions) { 24 } else { 4 } << 10;
const STACK_BASE: usize = 8 << 20;

/// `source` as JavaScript; Err is why it could not be (a syntax error, first
/// diagnostic first, or a source too long to strip safely). swc runs on a
/// thread of its own, with a stack for the deepest this source can nest.
pub fn strip_types(source: &str, filename: &str) -> Result<String, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "{} KB of TypeScript is past the {} KB kern strips; write it as JavaScript, or split it",
            source.len() >> 10,
            MAX_SOURCE_BYTES >> 10
        ));
    }
    let (owned, name) = (source.to_string(), filename.to_string());
    std::thread::Builder::new()
        .name("kern-ts".into())
        .stack_size(STACK_BASE + source.len() * STACK_PER_BYTE)
        .spawn(move || strip_here(&owned, &name))
        .map_err(|e| format!("the TypeScript stripper could not start: {e}"))?
        .join()
        .map_err(|_| "the TypeScript stripper failed on this source".to_string())?
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
    fn on_a_worker_stack(source: String, filename: &'static str) -> Result<String, String> {
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
        assert!(on_a_worker_stack("(".repeat(200_000), "a.ts").is_err());
    }

    #[test]
    fn a_syntax_error_is_an_error() {
        assert!(strip_types("const = ;", "a.ts").is_err());
    }
}
