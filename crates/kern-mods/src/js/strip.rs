//! TypeScript to JavaScript for a `.ts` mod: kern's one type stripper
//! (`kern-ts`, swc's), so a mod's error lines are the file's own.

use std::path::Path;

pub(crate) fn strip_types(path: &Path, source: &str) -> Result<String, String> {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    kern_ts::strip_types(source, &name).map_err(|why| format!("it is not valid TypeScript: {why}"))
}
