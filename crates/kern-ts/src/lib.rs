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

/// `source` as JavaScript; Err is why it could not be (a syntax error, first diagnostic first).
pub fn strip_types(source: &str, filename: &str) -> Result<String, String> {
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

    #[test]
    fn a_syntax_error_is_an_error() {
        assert!(strip_types("const = ;", "a.ts").is_err());
    }
}
