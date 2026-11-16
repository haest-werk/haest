//! A script file as the engine runs it: TypeScript by its extension and
//! stripped first, JSX refused.

use std::path::Path;

fn extension(path: &Path) -> &str {
    path.extension().and_then(|e| e.to_str()).unwrap_or("")
}

fn is_typescript(path: &Path) -> bool {
    matches!(extension(path), "ts" | "mts" | "cts")
}

/// A JSX file: the engine runs no JSX, and the type stripper leaves it in.
pub(super) fn is_jsx(path: &Path) -> bool {
    matches!(extension(path), "tsx" | "mtsx" | "ctsx" | "jsx")
}

/// The file as JavaScript. Stripping blocks for as long as it parses, so it
/// runs on tokio's blocking pool, not on the shell call's worker.
#[cfg(feature = "typescript")]
pub(super) async fn runnable(source: String, path: &Path) -> Result<String, String> {
    if !is_typescript(path) {
        return Ok(source);
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let strip = tokio::task::spawn_blocking(move || haest_ts::strip_types(&source, &name));
    match strip.await {
        Ok(stripped) => stripped.map_err(|e| haest_core::error::report(&e)),
        Err(_) => Err(haest_ts::StripError::Panicked.to_string()),
    }
}

#[cfg(not(feature = "typescript"))]
pub(super) async fn runnable(source: String, path: &Path) -> Result<String, String> {
    if is_typescript(path) {
        return Err("TypeScript is not supported in this build; write JavaScript".into());
    }
    Ok(source)
}
